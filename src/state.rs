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
    *req.headers_mut() = copy_headers!(request, default_headers(), {
        set [ header::UserAgent::new(opts.user_agent.clone()) ],
        if_present [ header::AcceptEncoding ],
        or_default [ header::Accept::image() ]
    });
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

macro_rules! proxy_try {
    ( $result: expr ) => {
        match $result {
            Err(err) => return wrap(err),
            Ok(val) => val,
        }
    }
}

#[inline]
fn wrap<T>(t: T) -> Box<Future<Item = Response, Error = ::hyper::Error>>
where T: Into<Response>  {
    Box::new(::futures::future::ok(t.into()))
}

/// Handle the initial proxy request from a client. Returns a future that contains
/// the response to the initial request.
pub(crate) fn handle_proxy_request(client: Arc<ProxyClient>, options: Arc<EnvOptions>, request: Request) -> Box<Future<Item = Response, Error = ::hyper::Error>> {
    // TODO(benl): check method and path and stuff
    let target_uri = proxy_try!(get_target_uri(&request));
    let redirects = options.max_number_redirects;
    proxy_it(client, options, request, target_uri, redirects)
}

/// Handles a request to make a proxy request to an upstream URI. Returns a
/// future for making that request and transforming the response into a
/// response back to the original client.
fn proxy_it(client: Arc<ProxyClient>, options: Arc<EnvOptions>, request: Request, to: Uri, retries_remaining: usize) -> Box<Future<Item = Response, Error = ::hyper::Error>> {
    let upstream_request = proxy_try!(client.request(build_proxy_request(&request, to, &options)));
    let work = upstream_request.then(move |upstream_result| {
        match upstream_result {
            Err(_) => wrap(ProxyError::RequestFailed),
            Ok(response) => {
                match response.status().as_u16() {
                    // if it's a redirect and we're outta patience, return an erro
                    301 | 302 | 303 | 307 if retries_remaining == 0 => wrap(ProxyError::TooManyRedirects),
                    // if it's a redirect, try to do it
                    301 | 302 | 303 | 307 => match get_redirect_uri(&response) {
                        Err(proxy_err) => wrap(proxy_err),
                        Ok(next_uri) => proxy_it(client, options, request, next_uri, retries_remaining - 1),
                    },
                    // otherwise, cleanup the response and return it
                   _ => wrap(build_proxy_response(response, &options)),
                }
            },
        }
    });
    Box::new(work)
}
