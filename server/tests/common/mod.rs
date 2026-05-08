#![allow(dead_code)]
//! Shared harness for integration tests.
//!
//! Spins up a single Postgres container per test process via `testcontainers`
//! (requires Docker on the host), runs the migrations once on a setup pool,
//! and hands each test a fresh, dedicated `PgPool` plus a real axum server
//! bound to a random local port. Tests are serialized via `#[serial]` so the
//! shared `kv_*` tables can be truncated between runs.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ferra_server::{api, config::Config, db, state::AppState};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use testcontainers::ContainerAsync;
use testcontainers_modules::{
    postgres::Postgres as PgImage,
    testcontainers::runners::AsyncRunner,
};
use tokio::net::TcpListener;
use tokio::sync::OnceCell;

struct SharedPg {
    _container: ContainerAsync<PgImage>,
    url: String,
}

static SHARED: OnceCell<SharedPg> = OnceCell::const_new();

async fn shared() -> &'static SharedPg {
    SHARED
        .get_or_init(|| async {
            let container = PgImage::default()
                .start()
                .await
                .expect("start postgres container");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("get postgres host port");
            let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

            // Run migrations once via a setup pool. Goes through `db::connect`
            // so that public DB-layer code is exercised by the integration suite.
            let setup_pool = db::connect(&url).await.expect("db::connect for setup");
            db::migrate(&setup_pool).await.expect("run migrations");
            setup_pool.close().await;

            SharedPg {
                _container: container,
                url,
            }
        })
        .await
}

async fn fresh_pool() -> PgPool {
    let pg = shared().await;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&pg.url)
        .await
        .expect("connect dedicated test pool");
    sqlx::query("TRUNCATE kv_events, kv_configs RESTART IDENTITY")
        .execute(&pool)
        .await
        .expect("truncate tables");
    pool
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub pool: PgPool,
    pub client: reqwest::Client,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl TestServer {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, self.url(path))
    }
}

pub struct StartOptions {
    pub max_value_bytes: usize,
    pub heartbeat: Duration,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            max_value_bytes: 1024,
            heartbeat: Duration::from_millis(150),
        }
    }
}

pub async fn start() -> TestServer {
    start_with(StartOptions::default()).await
}

pub async fn start_with(opts: StartOptions) -> TestServer {
    let pool = fresh_pool().await;

    let cfg = Config {
        database_url: "ignored-by-tests".into(),
        http_addr: "127.0.0.1:0".into(),
        max_value_bytes: opts.max_value_bytes,
        watch_heartbeat: opts.heartbeat,
    };
    let state = Arc::new(AppState::new(cfg, pool.clone()));
    let app = api::router(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind random port");
    let addr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    TestServer {
        addr,
        pool,
        client: reqwest::Client::new(),
        handle,
    }
}
