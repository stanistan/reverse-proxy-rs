extern crate futures;
extern crate hyper;
extern crate url;

use futures::future::{Future, FutureResult};
use hyper::header::ContentLength;
use hyper::server::{Http, Request, Response, Service};
use hyper::{Method, StatusCode};
use url::{form_urlencoded, Url};

#[derive(Debug)]
enum ProxyError {
    NoQueryParameter,
    InvalidUrl { url: String },
    Wat, // fixme
}

struct Log<S> {
    wrapped: S
}

impl<S> Service for Log<S> where S: Service<Request=Request, Response=Response, Error=hyper::Error> {
    type Request = S::Request;
    type Response = S::Response;
    type Error = S::Error;

    type Future = futures::Then<
        S::Future,
        FutureResult<S::Response, S::Error>,
        FnOnce(Result<S::Response, S::Error>) -> FutureResult<Self::Response, Self::Error>
    >;


    fn call(&self, r: Self::Request) -> Self::Future {
        self.wrapped.call(r).then(|re| {
            let request_path = "";
            match re {
                Ok(ref response) => {
                    println!("OK  {} {}", request_path, response.status().as_u16());
                },
                Err(ref e) => {
                    println!("ERR {} {:?}", request_path, e);
                }
            };
            futures::future::result(re)
        })
    }
}


struct ReverseProxy;

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;

    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        let mut response = Response::new();
        match req.method() {
            &Method::Get => match get_target_url(&req) {
                Err(proxy_error) => {
                    println!("{:?}", proxy_error);
                    response.set_status(StatusCode::BadRequest);
                }
                Ok(url) => {
                    let body = format!("{:?}", url);
                    println!("{}", body);
                    response = response.with_header(ContentLength(body.len() as u64));
                    response.set_body(body);
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
        None => return Err(ProxyError::NoQueryParameter),
    };

    let param = form_urlencoded::parse(query.as_bytes()).find(|(k, v)| k == "q");
    match param {
        None => Err(ProxyError::NoQueryParameter),
        Some((_, v)) => Url::parse(&v).map_err(|_| ProxyError::InvalidUrl { url: v.to_string() }),
    }
}

fn main() {
    let addr = "127.0.0.1:3000".parse().unwrap();
    let server = Http::new().bind(&addr, || Ok(Log { wrapped: ReverseProxy })).unwrap();
    server.run().unwrap();
}
