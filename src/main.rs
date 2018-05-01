extern crate futures;
extern crate hyper;
extern crate net2;
extern crate tokio_core;
extern crate url;

use futures::future::Future;
use futures::stream::Stream;
use hyper::client::HttpConnector;
use hyper::header::Location;
use hyper::server::{Http, Response, Service};
use hyper::{Chunk, Client, Method, Request, StatusCode, Uri};
use net2::TcpBuilder;
use net2::unix::UnixTcpBuilderExt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;
use url::{form_urlencoded, Url};

static QUERY_PARAM: &'static str = "q";
static MAX_NUM_RETRIES: usize = 3;
static PROXY_THREADS: usize = 4;

/// Describes the different states that the proxy
/// service can be in. Each of the variants will contain
/// the initial request so we can do tracing/logging/debugging.
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

impl ProxyRequestState {
    fn next(
        self,
        client: Arc<Client<HttpConnector>>,
    ) -> Box<Future<Item = (Request, Response), Error = hyper::Error>> {
        use ProxyRequestState::*;

        match self {
            Done { request, response } => {
                println!("{} {}", request.uri(), response.status().as_u16());
                Box::new(futures::future::ok((request, response)))
            }
            Invalid { request, err } => {
                println!("proxy_error: {}: {:?}", request.uri(), err);
                let mut response = Response::new();
                response.set_status(StatusCode::BadRequest);
                response.set_body(format!("{:?}", err));
                ProxyRequestState::next(Done { request, response }, client)
            }
            Incoming { request } => ProxyRequestState::next(
                match get_target_uri(&request) {
                    Err(err) => Invalid { request, err },
                    Ok(to) => Proxy {
                        request,
                        to,
                        retries_remaining: MAX_NUM_RETRIES,
                    },
                },
                client,
            ),
            Proxy {
                request,
                to: _,
                retries_remaining: 0,
            } => ProxyRequestState::next(
                Invalid {
                    request: request,
                    err: ProxyError::TooManyRedirects,
                },
                client,
            ),
            Proxy {
                request,
                to,
                retries_remaining,
            } => {
                let work = client.request(Request::new(Method::Get, to)).and_then(
                    move |response| {
                        ProxyRequestState::next(
                            match response.status().is_redirection() {
                                true => match get_redirect_uri(&response) {
                                    Ok(to) => Proxy {
                                        request,
                                        to,
                                        retries_remaining: retries_remaining - 1,
                                    },
                                    Err(err) => Invalid { request, err },
                                },
                                _ => Done { request, response },
                            },
                            client,
                        )
                    },
                );
                Box::new(work)
            }
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

struct ReverseProxy {
    client: Arc<Client<HttpConnector>>,
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Response, Error = hyper::Error>>;

    fn call(&self, request: Request) -> Self::Future {
        let work = ProxyRequestState::Incoming { request }
            .next(self.client.clone())
            .map(|(_, response)| response);
        Box::new(work)
    }
}

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
        .find(|(k, _)| k == QUERY_PARAM)
        .ok_or_else(|| ProxyError::NoQueryParameter)?;
    let url = Url::parse(&query_param).map_err(|_| ProxyError::InvalidUrl)?;
    Uri::from_str(url.as_str()).map_err(|_| ProxyError::InvalidUrl)
}

fn serve(worker_id: usize, addr: SocketAddr) {
    let mut core = Core::new().expect(&format!("thread-{}: error: no core for you", worker_id));
    let server_handle = core.handle();
    let client_handle = core.handle();

    let tcp = TcpBuilder::new_v4().unwrap()
        .reuse_address(true).unwrap()
        .reuse_port(true).unwrap()
        .bind(addr).unwrap()
        .listen(128).unwrap();

    let listener = TcpListener::from_listener(tcp, &addr, &server_handle).unwrap();
    let http: Http<Chunk> = Http::new();
    let client = Arc::new(Client::configure().build(&client_handle));

    core.run(listener.incoming().for_each(|(data, _addr)| {
        http.serve_connection(data, ReverseProxy{client: client.clone()})
            .map_err(|_| std::io::Error::last_os_error()) // FIXME wat
    })).unwrap();
}

fn main() {
    let addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();

    for worker_id in 1..PROXY_THREADS {
        let addr = addr.clone();
        thread::spawn(move || serve(worker_id, addr));
    }

    serve(0, addr);
}
