#![allow(dead_code, unused_imports, unused_variables)]
extern crate futures;
extern crate hyper;
extern crate tokio_core;
extern crate url;

use futures::future::{AndThen, Future, FutureResult, OrElse};
use futures::stream::Stream;
use hyper::client::HttpConnector;
use hyper::header::{ContentLength, Headers};
use hyper::server::{Http, Response, Service};
use hyper::{Client, Method, Request, StatusCode, Uri};
use std::sync::Arc;
use tokio_core::reactor::{Core, Handle};
use url::{form_urlencoded, Url};

#[derive(Debug)]
enum ProxyError {
    NoQueryParameter,
    InvalidUrl { url: String },
    Wat, // fixme
}

struct ReverseProxy {
    client: Arc<Client<HttpConnector>>,
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;

    type Future = BoxFuture;

    fn call(&self, req: Request) -> Self::Future {
        let client = self.client.clone();
        let proxy_request = proxy_incoming_request(req, client)
                .and_then(proxy_outgoing_request)
                .or_else(|err| {
                    println!("fuck: {:?}", err);
                    let response = Response::new().with_status(StatusCode::InternalServerError);
                    Ok(response)
                });
        Box::new(proxy_request)
    }
}

type BoxFuture = Box<Future<Item = Response, Error = hyper::Error>>;


// start handling an incoming https request from a client, translate it into an outgoing upstream
// http(s) request, and return a future for that request
fn proxy_incoming_request(request: Request, client: Arc<Client<HttpConnector>>) -> BoxFuture {
    use std::str::FromStr;
    if request.method() != &Method::Get {
        let mut response = Response::new();
        response.set_status(StatusCode::MethodNotAllowed);
        return Box::new(futures::future::ok(response));
    }

    let url = match get_target_url(&request) {
        Err(proxy_error) => {
            println!("{:?}", proxy_error); // debooglin

            let mut response = Response::new();
            response.set_status(StatusCode::BadRequest);
            return Box::new(futures::future::ok(response));
        }
        Ok(url) => url,
    };

    let request = Request::new(
        Method::Get,
        Uri::from_str(&url.as_str()).expect("fuck this"),
    );
    println!("proxy_incoming_request: {:?}", request);
    Box::new(client.request(request))
}

// handle a response from an upstream host and, if it's valid, return a future that transfers data
// from the upstream request back to the client (with appropriate HTTP headers)
fn proxy_outgoing_request(proxy_response: Response) -> BoxFuture {
    if proxy_response.status().is_redirection() {
        unimplemented!()
    }
    println!("proxy_outgoing_request: {:?}", proxy_response);

    Box::new(futures::future::ok(
        proxy_response.with_headers(Headers::new()),
    ))
}

fn get_target_url(request: &Request) -> Result<Url, ProxyError> {
    let query = match request.query() {
        Some(query_str) => query_str,
        None => return Err(ProxyError::NoQueryParameter),
    };

    let param = form_urlencoded::parse(query.as_bytes()).find(|(k, v)| k == "q");
    match param {
        None => Err(ProxyError::NoQueryParameter),
        Some((_, v)) => Url::parse(&v).map_err(|_| ProxyError::InvalidUrl { url: v.to_string() }),
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
