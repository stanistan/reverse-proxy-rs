extern crate futures;
extern crate hyper;
extern crate net2;
extern crate tokio_core;
extern crate url;

use futures::future::Future;
use futures::stream::Stream;
use hyper::client::HttpConnector;
use hyper::server::{Http, Response, Service};
use hyper::{Chunk, Client, Request};
use net2::unix::UnixTcpBuilderExt;
use net2::TcpBuilder;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;

mod state;

use state::ProxyRequestState;

static DEFAULT_MAX_REDIRECTS: usize = 4;
static DEFAULT_PROXY_THREADS: usize = 4;
static DEFAULT_PORT: usize = 3000;

#[derive(Debug)]
/// Describes the ways that the Proxy server can fail.
enum ProxyError {
    NoQueryParameter,
    InvalidUrl,
    TooManyRedirects,
    BadRedirect,
}

struct ReverseProxy {
    client: Arc<Client<HttpConnector>>,
    options: Arc<EnvOptions>,
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Response, Error = hyper::Error>>;

    fn call(&self, request: Request) -> Self::Future {
        let work = ProxyRequestState::Incoming { request }
            .process(self.client.clone(), self.options.max_number_redirects)
            .map(|(_, response)| response);
        Box::new(work)
    }
}

fn serve(worker_id: usize, options: Arc<EnvOptions>) {
    let mut core = Core::new().expect(&format!("thread-{}: error: no core for you", worker_id));
    let server_handle = core.handle();
    let client_handle = core.handle();

    let tcp = TcpBuilder::new_v4()
        .unwrap()
        .reuse_address(true)
        .unwrap()
        .reuse_port(true)
        .unwrap()
        .bind(&options.addr)
        .unwrap()
        .listen(128)
        .unwrap();

    let listener = TcpListener::from_listener(tcp, &options.addr, &server_handle).unwrap();
    let http: Http<Chunk> = Http::new();
    let client = Arc::new(Client::configure().build(&client_handle));

    core.run(listener.incoming().for_each(|(data, _addr)| {
        http.serve_connection(
            data,
            ReverseProxy {
                client: client.clone(),
                options: options.clone(),
            },
        ).map_err(|_| std::io::Error::last_os_error()) // FIXME wat
    })).unwrap();
}

#[derive(Debug)]
struct EnvOptions {
    addr: SocketAddr,
    num_threads: usize,
    max_number_redirects: usize,
}

impl EnvOptions {
    /// This function *will panic* if we couldn't parse
    /// the environment correctly.
    fn collect() -> EnvOptions {
        use std::collections::HashMap;
        let env_vars: HashMap<String, String> = std::env::vars().collect();
        macro_rules! env {
            ($k: expr, $d: expr) => {
                env_vars.get(stringify!($k))
                    .map(|v| v.parse().expect(&format!("Error parsing {}", stringify!($k))))
                    .unwrap_or($d)
            };
            ($k: expr) => {
                env!($k, "")
            };
        };

        let port: usize = env!(PORT, DEFAULT_PORT);
        let num_threads: usize = env!(NUM_THREADS, DEFAULT_PROXY_THREADS);
        let max_number_redirects: usize = env!(MAX_REDIRECTS, DEFAULT_MAX_REDIRECTS);

        let addr: SocketAddr = format!("127.0.0.1:{}", port)
            .parse()
            .expect("Erorr parsing socket address for PORT");

        EnvOptions {
            addr,
            num_threads,
            max_number_redirects,
        }
    }
}

fn main() {
    let options = Arc::new(EnvOptions::collect());
    println!("Starting server with options: {:?}", options);

    for worker_id in 1..options.num_threads {
        let options = options.clone();
        thread::spawn(move || serve(worker_id, options));
    }

    serve(0, options);
}
