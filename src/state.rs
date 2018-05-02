use futures::future::Future;
use hyper::{Method, Request, Response, Uri};
use hyper::header;
use std::sync::Arc;
use std::str::FromStr;
use super::{EnvOptions, ProxyClient, ProxyError};
use url::{form_urlencoded, Url};

static QUERY_PARAM: &'static str = "q";

header! { (XFrameOptions, "X-Frame-Options") => [String] }
header! { (XXSSProtection, "X-XSS-Protection") => [String] }
header! { (XContentTypeOptions, "X-Content-Type-Options") => [String] }
header! { (ContentSecurityPolicy, "Content-Security-Policy") => [String] }
header! { (StrictTransportSecurity, "Strict-Transport-Security") => [String] }

macro_rules! copy_headers {
    (
        $from: expr, $to: expr, {
            set [ $($set:expr),* ],
            if_present [ $($t:ty),* ],
            or_default [ $($else:expr),* ]
        }
    ) => {{
        let mut to = $to;
        $({
            to.set($set);
        })*
        $({
            to.set($from.headers().get().cloned().unwrap_or_else(|| $else));
        })*
        $({
            if let Some(v) = $from.headers().get::<$t>() {
                to.set(v.clone());
            }
        })*
        to
    }}
}

/// Attempt to parse out the redirect uri from the proxy response.
fn get_redirect_uri(response: &Response) -> Result<Uri, ProxyError> {
    let location: Option<&header::Location> = response.headers().get();
    location
        .and_then(|l| Uri::from_str(&*l).ok())
        .ok_or_else(|| ProxyError::BadRedirect)
}

fn default_headers() -> header::Headers {
    copy_headers!((), header::Headers::new(), {
        set [
            XFrameOptions("deny".to_owned()),
            XXSSProtection("1; mode=block".to_owned()),
            XContentTypeOptions("nosniff".to_owned()),
            ContentSecurityPolicy("default-src 'none'; img-src data:; style-src 'unsafe-inline'"
                                  .to_owned()),
            StrictTransportSecurity("max-age=31536000; includeSubDomains".to_owned())
        ],
        if_present [ ],
        or_default [ ]
    })
}

fn build_proxy_request(request: &Request, to: Uri, opts: &EnvOptions) -> Request {
    let mut req = Request::new(Method::Get, to);
    {
        let h = copy_headers!(request, default_headers(), {
            set [ header::UserAgent::new(opts.user_agent.clone()) ],
            if_present [ header::AcceptEncoding ],
            or_default [ header::Accept::image() ]
        });
        let req_headers = req.headers_mut();
        *req_headers = h;
    }
    req
}

fn build_proxy_response(response: Response, options: &EnvOptions) -> Response {

    let headers = copy_headers!(response, default_headers(), {
        set [ ],
        if_present [
            header::ContentType,
            header::ETag,
            header::Expires,
            header::LastModified,
            header::ContentLength,
            header::TransferEncoding,
            header::ContentEncoding
        ],
        or_default [
            header::CacheControl(vec![
                header::CacheDirective::Public,
                header::CacheDirective::MaxAge(31536000)
            ])
        ]
    });

    let response = response.with_headers(headers);
    if !response.status().is_success() {
        return response;
    }

    // ensure we have a present and valid content type
    if let Some(ct) = response.headers().get::<header::ContentType>() {
        // FIXME: content type -> str stuff
        // is pretty bananas with the mime type/subtype/suffix
        // :shrug:
        if !options.is_valid_content_type(&format!("{}", ct)) {
            return ProxyError::InvalidContentType.into();
        }
    } else {
        return ProxyError::InvalidContentType.into();
    }

    response

}

/// Attempt to extract the proxy target's URI from the original
/// incoming request to the service.
fn get_target_uri(request: &Request) -> Result<Uri, ProxyError> {
    let query = request.query().ok_or_else(|| ProxyError::NoQueryParameter)?;
    let (_, query_param) = form_urlencoded::parse(query.as_bytes())
        .find(|&(ref k, _)| k == QUERY_PARAM)
        .ok_or_else(|| ProxyError::NoQueryParameter)?;
    let url = Url::parse(&query_param).map_err(|_| ProxyError::InvalidUrl)?;
    Uri::from_str(url.as_str()).map_err(|_| ProxyError::InvalidUrl)
}

/// Describes the different states that the proxy
/// service can be in. Each of the variants will contain
/// the initial request so we can do tracing/logging/debugging.
pub(crate) enum ProxyRequestState {
    Incoming {
        request: Request,
    },
    Invalid {
        request: Request,
        err: ProxyError,
    },
    Done {
        request: Request,
        response: Response,
    },
    Proxy {
        request: Request,
        to: Uri,
        retries_remaining: usize,
    },
    ProxyProcessing {
        request: Request,
        response: Response,
        retries_remaining: usize,
    },
}

impl ProxyRequestState {
    /// Process the state. This function will recurse with state
    /// transitions until we get to `Done`.
    pub(crate) fn process(
        self,
        client: Arc<ProxyClient>,
        options: Arc<EnvOptions>,
    ) -> Box<Future<Item = (Request, Response), Error = ::hyper::Error>> {
        use ProxyRequestState::*;

        match self {
            // This is the final state of the machine.
            // We have processed `Incoming` all the way through.
            Done { request, response } => {
                println!("{} {}", request.uri(), response.status().as_u16());
                Box::new(::futures::future::ok((request, response)))
            }
            // An invalid state will be transformed into a response
            // to be output back to the user.
            Invalid { request, err } => {
                println!("proxy_error: {}: {:?}", request.uri(), err);
                let response = err.into();
                ProxyRequestState::process(Done { request, response }, client, options)
            }
            // The incoming request gets validated and we continue on.
            Incoming { request } => ProxyRequestState::process(
                // TODO this should probably be creating a new request?
                // we need to set headers correctly (including user-agent)
                // for when we make the proxy request in the `Proxy`
                // state.
                match get_target_uri(&request) {
                    Err(err) => Invalid { request, err },
                    Ok(to) => Proxy {
                        request,
                        to,
                        retries_remaining: options.max_number_redirects,
                    },
                },
                client,
                options,
            ),
            // State during which we are processing the response from
            // the proxy.
            ProxyProcessing {
                request,
                response,
                retries_remaining,
            } => ProxyRequestState::process(
                match response.status().as_u16() {
                    301 | 302 | 303 | 307 => match get_redirect_uri(&response) {
                        Err(err) => Invalid { request, err },
                        Ok(to) => Proxy {
                            request,
                            to,
                            retries_remaining: retries_remaining - 1,
                        },
                    },
                    /*304 => Done {
                        request,
                        response: build_proxy_response_no_body(response, &options),
                    },*/
                    _ => Done {
                        request,
                        response: build_proxy_response(response, &options),
                    },
                },
                client,
                options,
            ),
            // We have followed redirects until we can no longer followed
            // redirects. :(
            Proxy {
                request,
                to: _,
                retries_remaining: 0,
            } => ProxyRequestState::process(
                Invalid {
                    request: request,
                    err: ProxyError::TooManyRedirects,
                },
                client,
                options,
            ),
            // This is where we do the main processing of making the request
            // and actually acting as a proxy.
            //
            // This can loop back into itself as we follow redirects.
            Proxy {
                request,
                to,
                retries_remaining,
            } => match client.request(build_proxy_request(&request, to, &options)) {
                Ok(request_future) => Box::new(request_future.and_then(move |response| {
                    ProxyRequestState::process(
                        ProxyProcessing {
                            request,
                            response,
                            retries_remaining,
                        },
                        client,
                        options,
                    )
                })),
                Err(err) => ProxyRequestState::process(Invalid { request, err }, client, options),
            },
        }
    }
}
