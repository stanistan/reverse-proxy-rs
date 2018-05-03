extern crate futures;
#[macro_use]
extern crate hyper;
extern crate hyper_tls;
extern crate net2;
extern crate tokio_core;
extern crate url;

use futures::future::Future;
use futures::stream::Stream;
use hyper::client::HttpConnector;
use hyper::server::{Http, Response, Service};
use hyper::{Chunk, Client, Request};
use hyper_tls::HttpsConnector;
use net2::unix::UnixTcpBuilderExt;
use net2::TcpBuilder;
use std::sync::Arc;
use std::thread;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;

mod env_options;
mod mime_types;
mod state;

use env_options::EnvOptions;
use state::{handle_proxy_request};

#[derive(Debug)]
/// Describes the ways that the Proxy server can fail.
enum ProxyError {
    NoQueryParameter,
    InvalidUrl,
    TooManyRedirects,
    BadRedirect,
    InvalidContentType,
    RequestFailed,
}

impl From<ProxyError> for Response {
    fn from(err: ProxyError) -> Response {
        let mut response = Response::new();
        response.set_status(hyper::StatusCode::BadRequest);
        response.set_body(format!("{:?}", err));
        response
    }
}

struct ProxyClient {
    http: Client<HttpConnector>,
    https: Client<HttpsConnector<HttpConnector>>,
}

impl ProxyClient {
    fn request(
        &self,
        request: Request,
    ) -> Result<Box<Future<Item = Response, Error = hyper::Error>>, ProxyError> {
        match request.uri().scheme() {
            Some("https") => Ok(Box::new(self.https.request(request))),
            Some("http") => Ok(Box::new(self.http.request(request))),
            _ => Err(ProxyError::InvalidUrl),
        }
    }
}

struct ReverseProxy {
    client: Arc<ProxyClient>,
    options: Arc<EnvOptions>,
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Response, Error = hyper::Error>>;

    fn call(&self, request: Request) -> Self::Future {
        Box::new(handle_proxy_request(self.client.clone(), self.options.clone(), request))
    }
}

fn serve(worker_id: usize, options: Arc<EnvOptions>) {
    let mut core = Core::new().expect(&format!("thread-{}: error: no core for you", worker_id));
    let server_handle = core.handle();
    let client_handle = core.handle();

    let reuse = options.num_threads > 1;
    let tcp = TcpBuilder::new_v4()
        .unwrap()
        .reuse_address(reuse)
        .unwrap()
        .reuse_port(reuse)
        .unwrap()
        .bind(&options.addr)
        .unwrap()
        .listen(128)
        .unwrap();

    let listener = TcpListener::from_listener(tcp, &options.addr, &server_handle).unwrap();
    let http: Http<Chunk> = Http::new();
    let http_client = Client::configure().build(&client_handle);
    let https_client = Client::configure()
        .connector(HttpsConnector::new(4, &client_handle).unwrap())
        .build(&client_handle);

    let client = Arc::new(ProxyClient {
        http: http_client,
        https: https_client,
    });

    println!("Starting worker {}", worker_id);
    core.run(listener.incoming().for_each(|(data, _addr)| {
        http.serve_connection(
            data,
            ReverseProxy {
                client: client.clone(),
                options: options.clone(),
            },
        ).map_err(|_| std::io::Error::last_os_error()) // FIXME wat but furreal
    })).unwrap();
}

fn main() {
    let options = Arc::new(EnvOptions::create());
    println!(
        "Starting server: num_threads={} max_number_redirects={} addr={}",
        options.num_threads, options.max_number_redirects, options.addr
    );

    for worker_id in 1..options.num_threads {
        let options = options.clone();
        thread::spawn(move || serve(worker_id, options));
    }

    serve(0, options);
}
