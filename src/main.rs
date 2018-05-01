extern crate futures;
extern crate hyper;
extern crate tokio_core;
extern crate url;

use futures::future::Future;
use futures::stream::Stream;
use hyper::client::HttpConnector;
use hyper::header::Location;
use hyper::server::{Http, Response, Service};
use hyper::{Client, Method, Request, StatusCode, Uri};
use std::str::FromStr;
use std::sync::Arc;
use tokio_core::reactor::Core;
use url::{form_urlencoded, Url};

enum ProxyRequestState {
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

type ProxyRequestFuture = Box<Future<Item = ProxyRequestState, Error = hyper::Error>>;

fn next_proxy_request_state(
    client: Arc<Client<HttpConnector>>,
    state: ProxyRequestState,
) -> ProxyRequestFuture {
    use ProxyRequestState::*;

    match state {
        Done { request, response } => {
            println!("{} {}", request.uri(), response.status().as_u16());
            Box::new(futures::future::ok(Done { request, response }))
        }
        Invalid { request, err } => {
            println!("proxy_error: {}: {:?}", request.uri(), err);
            let mut response = Response::new();
            response.set_status(StatusCode::BadRequest);
            response.set_body(format!("{:?}", err));
            next_proxy_request_state(client, Done { request, response })
        }
        Incoming { request } => next_proxy_request_state(
            client,
            match get_target_url(&request) {
                Err(err) => Invalid { request, err },
                Ok(url) => match Uri::from_str(url.as_str()) {
                    Ok(to) => Proxy {
                        request,
                        to,
                        retries_remaining: 3,
                    },
                    Err(_) => Invalid {
                        request,
                        err: ProxyError::InvalidUrl,
                    },
                },
            },
        ),
        Proxy {
            request,
            to: _,
            retries_remaining: 0,
        } => next_proxy_request_state(
            client,
            Invalid {
                request: request,
                err: ProxyError::TooManyRedirects,
            },
        ),
        Proxy {
            request,
            to,
            retries_remaining,
        } => {
            let work = client
                .request(Request::new(Method::Get, to))
                .and_then(move |response| {
                    next_proxy_request_state(
                        client,
                        match response.status().is_redirection() {
                            true => match get_redirect_url(&response) {
                                Some(to) => Proxy {
                                    request,
                                    to,
                                    retries_remaining: retries_remaining - 1,
                                },
                                _ => Invalid {
                                    request,
                                    err: ProxyError::BadRedirect,
                                },
                            },
                            _ => Done { request, response },
                        },
                    )
                });
            Box::new(work)
        }
    }
}

#[derive(Debug)]
enum ProxyError {
    NoQueryParameter,
    InvalidUrl,
    TooManyRedirects,
    BadRedirect,
}

type BoxFuture = Box<Future<Item = Response, Error = hyper::Error>>;

struct ReverseProxy {
    client: Arc<Client<HttpConnector>>,
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = BoxFuture;

    fn call(&self, request: Request) -> Self::Future {
        let client = self.client.clone();
        let work = next_proxy_request_state(client, ProxyRequestState::Incoming { request }).map(
            |state| {
                if let ProxyRequestState::Done { request: _ , response } = state {
                    response
                } else {
                    panic!("not a terminal state")
                }
            },
        );
        Box::new(work)
    }
}

fn get_redirect_url(response: &Response) -> Option<Uri> {
    let location: Option<&Location> = response.headers().get();
    location.and_then(|l| Uri::from_str(&*l).ok())
}

fn get_target_url(request: &Request) -> Result<Url, ProxyError> {
    let query = match request.query() {
        Some(query_str) => query_str,
        None => return Err(ProxyError::NoQueryParameter),
    };

    let param = form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == "q");
    match param {
        None => Err(ProxyError::NoQueryParameter),
        Some((_, v)) => Url::parse(&v).map_err(|_| ProxyError::InvalidUrl),
    }
}

fn main() {
    let mut core = Core::new().expect("error: no core for you");
    let addr = "127.0.0.1:3000".parse().unwrap();

    let server_handle = core.handle();
    let client_handle = core.handle();

    let serve = Http::new()
        .serve_addr_handle(&addr, &server_handle, move || {
            Ok(ReverseProxy {
                client: Arc::new(Client::configure().build(&client_handle)),
            })
        })
        .unwrap();

    let h2 = server_handle.clone();
    server_handle.spawn(
        serve
            .for_each(move |conn| {
                h2.spawn(
                    conn.map(|_| ())
                        .map_err(|err| println!("serve err: {:?}", err)),
                );
                Ok(())
            })
            .map_err(|_| ()),
    );

    core.run(futures::future::empty::<(), ()>()).unwrap();
}
