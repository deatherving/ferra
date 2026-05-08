use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ferra_agent=info,tower_http=info".into()),
        )
        .init();

    let args = ferra_agent::Args::parse();
    ferra_agent::run(args).await
}
