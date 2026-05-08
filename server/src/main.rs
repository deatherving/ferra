use ferra_server::{run, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ferra_server=info,tower_http=info".into()),
        )
        .init();

    let cfg = Config::from_env()?;
    run(cfg).await
}
