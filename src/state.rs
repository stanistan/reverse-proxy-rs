use futures::future::Future;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper::header;
use std::sync::Arc;
use std::str::FromStr;
use super::{EnvOptions, ProxyClient, ProxyError};
use url::{form_urlencoded, Url};

static QUERY_PARAM: &'static str = "q";

/// Attempt to parse out the redirect uri from the proxy response.
fn get_redirect_uri(response: &Response) -> Result<Uri, ProxyError> {
    let location: Option<&header::Location> = response.headers().get();
    location
        .and_then(|l| Uri::from_str(&*l).ok())
        .ok_or_else(|| ProxyError::BadRedirect)
}

fn build_proxy_request(request: &Request, to: Uri, options: &EnvOptions) -> Request {
    /*
    let mut h = default_headers();
    h.set(header::UserAgent::new(options.user_agent.clone()));
    h.set(request.headers().get().cloned().unwrap_or(header::Accept::image()));
    if let Some(encoding) = request.headers().get::<header::AcceptEncoding>() {
        h.set(encoding.clone());
    }
    */
    Request::new(Method::Get, to)

    // unimplemented!()
}

fn build_proxy_response(response: Response, options: &EnvOptions) -> Response {
    response
}

fn get_mime_type_prefix(response: &Response) -> Result<(), ProxyError> {
    println!("{:?}", response.headers().get::<header::ContentType>());
    Ok(())
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
                let mut response = Response::new();
                response.set_status(StatusCode::BadRequest);
                response.set_body(format!("{:?}", err));
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
            } => {
                let next_request = build_proxy_request(&request, to, &options);
                match client.request(next_request) {
                    Ok(request_future) => {
                        let work = request_future.and_then(move |response| {
                            ProxyRequestState::process(
                                match response.status().as_u16() {
                                    301 | 302 | 303 | 307 => match get_redirect_uri(&response) {
                                        Ok(to) => Proxy {
                                            request,
                                            to,
                                            retries_remaining: retries_remaining - 1,
                                        },
                                        Err(err) => Invalid { request, err },
                                    },
                                    _ => Done {
                                        request,
                                        response: build_proxy_response(response, &options),
                                    },
                                },
                                client,
                                options,
                            )
                        });
                        Box::new(work)
                    }
                    Err(err) => {
                        ProxyRequestState::process(Invalid { request, err }, client, options)
                    }
                }
            }
        }
    }
}
