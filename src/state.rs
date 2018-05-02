use futures::future::Future;
use hyper::{Client, Method, Request, Response, StatusCode, Uri};
use hyper::client::HttpConnector;
use hyper::header::Location;
use std::sync::Arc;
use std::str::FromStr;
use super::{EnvOptions, ProxyError};
use url::{form_urlencoded, Url};

static QUERY_PARAM: &'static str = "q";

/// Attempt to parse out the redirect uri from the proxy response.
fn get_redirect_uri(response: &Response) -> Result<Uri, ProxyError> {
    let location: Option<&Location> = response.headers().get();
    location
        .and_then(|l| Uri::from_str(&*l).ok())
        .ok_or_else(|| ProxyError::BadRedirect)
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
        client: Arc<Client<HttpConnector>>,
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
                let work = client.request(Request::new(Method::Get, to)).and_then(
                    move |response| {
                        ProxyRequestState::process(
                            match response.status().is_redirection() {
                                true => match get_redirect_uri(&response) {
                                    Ok(to) => Proxy {
                                        request,
                                        to,
                                        retries_remaining: retries_remaining - 1,
                                    },
                                    Err(err) => Invalid { request, err },
                                },
                                // TODO This should set the appropriate
                                // response/caching headers on the response
                                // that we want to send back out.
                                _ => Done { request, response },
                            },
                            client,
                            options,
                        )
                    },
                );
                Box::new(work)
            }
        }
    }
}
