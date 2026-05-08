pub mod api;
pub mod config;
pub mod db;
pub mod error;
pub mod events;
pub mod state;

pub use config::Config;

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let pool = db::connect(&cfg.database_url).await?;
    db::migrate(&pool).await?;

    let state = Arc::new(state::AppState::new(cfg.clone(), pool));
    let app = api::router(state);

    let addr: SocketAddr = cfg
        .http_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid FERRA_HTTP_ADDR {}: {e}", cfg.http_addr))?;
    info!(%addr, "ferra-server listening");

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown signal received");
}
