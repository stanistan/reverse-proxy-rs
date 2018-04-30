extern crate hyper;
extern crate futures;
extern crate url;

use futures::future::Future;
use hyper::header::ContentLength;
use hyper::server::{Http, Request, Response, Service};
use hyper::{Method, StatusCode};
use url::{form_urlencoded, Url};

#[derive(Debug)]
enum ProxyError {
    NoQueryParameter,
    InvalidUrl{ url: String },
    Wat, // fixme
}

struct ReverseProxy;

impl Service for ReverseProxy {

    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;

    type Future = Box<Future<Item=Self::Response, Error=Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        let mut response = Response::new();
        match req.method() {
            &Method::Get => {
                match get_target_url(&req) {
                    Err(proxy_error) => {
                        println!("{:?}", proxy_error);
                        response.set_status(StatusCode::BadRequest);
                    },
                    Ok(url) => {
                        let body = format!("{:?}", url);
                        println!("{}", body);
                        response = response.with_header(ContentLength(body.len() as u64));
                        response.set_body(body);
                    }
                }
            },
            _ => {
                response.set_status(StatusCode::MethodNotAllowed);
            }
        }

        Box::new(futures::future::ok(response))
    }

}

fn get_target_url(request: &Request) -> Result<Url, ProxyError> {
    let query = match request.query() {
        Some(query_str) => query_str,
        None => return Err(ProxyError::NoQueryParameter)
    };

    let param = form_urlencoded::parse(query.as_bytes()).find(|(k, v)| k == "q");
    match param {
        None => Err(ProxyError::NoQueryParameter),
        Some((_, v)) => {
            Url::parse(&v).map_err(|_| ProxyError::InvalidUrl { url: v.to_string() })
        }
    }
}

fn main() {
    let addr = "127.0.0.1:3000".parse().unwrap();
    let server = Http::new().bind(&addr, || Ok(ReverseProxy)).unwrap();
    server.run().unwrap();
}
