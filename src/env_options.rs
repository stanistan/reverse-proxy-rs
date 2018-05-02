use std::net::SocketAddr;
use std::collections::HashMap;
use std::collections::HashSet;

static DEFAULT_MAX_REDIRECTS: usize = 4;
static DEFAULT_PROXY_THREADS: usize = 4;
static DEFAULT_PORT: usize = 3000;
static DEFAULT_USER_AGENT: &'static str = "stanistan";

#[derive(Debug)]
pub(crate) struct EnvOptions {
    pub addr: SocketAddr,
    pub num_threads: usize,
    pub max_number_redirects: usize,
    pub user_agent: String,
    mime_types: HashSet<&'static str>,
}

impl EnvOptions {
    /// This function *will panic* if we couldn't parse
    /// the environment correctly.
    pub(crate) fn create() -> EnvOptions {
        let env_vars: HashMap<String, String> = ::std::env::vars().collect();
        macro_rules! env {
            ($k: expr, $d: expr) => {
                env_vars.get(stringify!($k))
                    .map(|v| v.parse().expect(&format!("Error parsing {}", stringify!($k))))
                    .unwrap_or_else(|| $d)
            };
        };

        let port: usize = env!(PORT, DEFAULT_PORT);

        let num_threads: usize = env!(NUM_THREADS, DEFAULT_PROXY_THREADS);
        assert!(num_threads > 0, "NUM_THREADS should always be > 0");

        let max_number_redirects: usize = env!(MAX_REDIRECTS, DEFAULT_MAX_REDIRECTS);

        let user_agent: String = env!(USER_AGENT, DEFAULT_USER_AGENT.to_owned());

        let addr: SocketAddr = format!("127.0.0.1:{}", port)
            .parse()
            .expect("Erorr parsing socket address for PORT");

        EnvOptions {
            addr,
            num_threads,
            max_number_redirects,
            user_agent,
            mime_types: ::mime_types::MIME_TYPES.iter().map(|s| *s).collect(),
        }
    }

    pub(crate) fn is_valid_content_type(&self, content_type: &str) -> bool {
        true
    }
}
