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

static QUERY_PARAM: &'static str = "q";
static MAX_NUM_RETRIES: usize = 3;
static PROXY_THREADS: usize = 4;

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
}

impl Service for ReverseProxy {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Response, Error = hyper::Error>>;

    fn call(&self, request: Request) -> Self::Future {
        let work = ProxyRequestState::Incoming { request }
            .process(self.client.clone())
            .map(|(_, response)| response);
        Box::new(work)
    }
}

fn serve(worker_id: usize, addr: SocketAddr) {
    let mut core = Core::new().expect(&format!("thread-{}: error: no core for you", worker_id));
    let server_handle = core.handle();
    let client_handle = core.handle();

    let tcp = TcpBuilder::new_v4()
        .unwrap()
        .reuse_address(true)
        .unwrap()
        .reuse_port(true)
        .unwrap()
        .bind(addr)
        .unwrap()
        .listen(128)
        .unwrap();

    let listener = TcpListener::from_listener(tcp, &addr, &server_handle).unwrap();
    let http: Http<Chunk> = Http::new();
    let client = Arc::new(Client::configure().build(&client_handle));

    core.run(listener.incoming().for_each(|(data, _addr)| {
        http.serve_connection(
            data,
            ReverseProxy {
                client: client.clone(),
            },
        ).map_err(|_| std::io::Error::last_os_error()) // FIXME wat
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
