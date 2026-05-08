//! Ferra sidecar agent.
//!
//! Runs alongside a service container, holds an in-memory cache of every key
//! under one or more configured prefixes, keeps that cache fresh via SSE
//! watches against `ferra-server`, and exposes a tiny localhost HTTP API the
//! service uses to read config:
//!
//! - `GET /cfg/{key}`                     — return current value or 404
//! - `GET /cfg/{key}?wait=30s&since=N`    — long-poll: return when key changes
//! - `GET /cfg?prefix=...`                — list everything under a prefix
//! - `GET /healthz`                       — process liveness
//! - `GET /readyz`                        — initial snapshots loaded?

pub mod api;
pub mod cache;
pub mod config;
pub mod sse;
pub mod upstream;
pub mod watch;

pub use config::Args;

use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

pub async fn run(args: Args) -> anyhow::Result<()> {
    if args.prefix.is_empty() {
        anyhow::bail!("at least one --prefix is required");
    }

    let upstream = upstream::UpstreamClient::new(args.server.clone());
    let prefixes: Vec<Arc<cache::PrefixState>> = args
        .prefix
        .iter()
        .map(|p| Arc::new(cache::PrefixState::new(p.clone())))
        .collect();

    // Spawn one watch task per prefix.
    for prefix in &prefixes {
        let prefix = prefix.clone();
        let upstream = upstream.clone();
        let min_backoff = args.min_backoff;
        let max_backoff = args.max_backoff;
        tokio::spawn(async move {
            watch::run_loop(prefix, upstream, min_backoff, max_backoff).await;
        });
    }

    let state = Arc::new(api::AgentState {
        upstream,
        prefixes: prefixes.clone(),
    });

    // Start the HTTP server right away so /healthz / /readyz are available
    // even before snapshots have loaded. Service containers can poll /readyz
    // to know when to start serving traffic.
    let app = api::router(state);
    let listener = TcpListener::bind(&args.listen).await?;
    let actual = listener.local_addr()?;
    info!(addr = %actual, prefixes = ?args.prefix, "ferra-agent listening");
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
