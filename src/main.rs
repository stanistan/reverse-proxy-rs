#![allow(dead_code, unused_imports, unused_variables)]
extern crate futures;
extern crate hyper;
extern crate url;

use futures::future::{Future, FutureResult};
use hyper::header::ContentLength;
use hyper::client::HttpConnector;
use hyper::server::{Http, Response, Service};
use hyper::{Client, Method, Request, StatusCode, Uri};
use url::{form_urlencoded, Url};

struct Context {
    request_path: String,
    // url: Option<Url>,
}

#[derive(Debug)]
enum ProxyError {
    NoQueryParameter,
    InvalidUrl { url: String },
    Wat, // fixme
}

struct ReverseProxy {
    client: Client<HttpConnector>
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;

    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        proxy_incoming_request(&req, &self.client)
    }
}

// FIXME these should have better type names
type BoxFuture = Box<Future<Item = Response, Error = hyper::Error>>;

// start handling an incoming https request from a client, translate it into an outgoing upstream
// http(s) request, and return a future for that request
fn proxy_incoming_request(request: &Request, client: &Client<HttpConnector>) -> BoxFuture {
    use std::str::FromStr;
    if request.method() != &Method::Get {
        let mut response = Response::new();
        response.set_status(StatusCode::MethodNotAllowed);
        return Box::new(futures::future::ok(response));
    }

    let url = match get_target_url(request) {
        Err(proxy_error) => {
            println!("{:?}", proxy_error); // debooglin

            let mut response = Response::new();
            response.set_status(StatusCode::BadRequest);
            return Box::new(futures::future::ok(response));
        }
        Ok(url) => url
    };

    Box::new(client.request(Request::new(Method::Get, Uri::from_str(&url.as_str()).expect("fuck this"))))
}

// handle a response from an upstream host and, if it's valid, return a future that transfers data
// from the upstream request back to the client (with appropriate HTTP headers)
fn proxy_outgoing_request() -> BoxFuture {
    // FIXME: do something
    unimplemented!()
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

    /*
    let addr = "127.0.0.1:3000".parse().unwrap();
    let client = Client::configure()
    let server = Http::new().bind(&addr, move || Ok(ReverseProxy {
        client
    })).unwrap();
    server.run().unwrap();
    */
}
