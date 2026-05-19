use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tracing::{error, info};

use crate::config::{DatabaseConfig, DiscreteDatabase, IamDatabase, PoolConfig};

/// Apply the operator-tunable pool settings (max/min connections, acquire
/// timeout, idle timeout, max lifetime) uniformly to all auth modes.
fn apply_pool_config(opts: PgPoolOptions, pool: &PoolConfig) -> PgPoolOptions {
    opts.max_connections(pool.max_connections)
        .min_connections(pool.min_connections)
        .acquire_timeout(pool.acquire_timeout)
        .idle_timeout(pool.idle_timeout)
        .max_lifetime(pool.max_lifetime)
}

pub async fn connect(cfg: &DatabaseConfig, pool: &PoolConfig) -> anyhow::Result<PgPool> {
    match cfg {
        DatabaseConfig::Url(url) => connect_url(url, pool).await,
        DatabaseConfig::Discrete(d) => connect_discrete(d, pool).await,
        DatabaseConfig::Iam(i) => connect_iam(i, pool).await,
    }
}

async fn connect_url(url: &str, pool: &PoolConfig) -> anyhow::Result<PgPool> {
    let pool = apply_pool_config(PgPoolOptions::new(), pool)
        .connect(url)
        .await?;
    Ok(pool)
}

fn discrete_options(d: &DiscreteDatabase) -> PgConnectOptions {
    let mut opts = PgConnectOptions::new()
        .host(&d.host)
        .port(d.port)
        .database(&d.name)
        .username(&d.user)
        .password(&d.password)
        .ssl_mode(d.ssl_mode);
    if let Some(ca) = d.ssl_root_cert.as_ref() {
        opts = opts.ssl_root_cert(ca);
    }
    opts
}

async fn connect_discrete(d: &DiscreteDatabase, pool: &PoolConfig) -> anyhow::Result<PgPool> {
    let opts = discrete_options(d);
    let pool = apply_pool_config(PgPoolOptions::new(), pool)
        .connect_with(opts)
        .await?;
    Ok(pool)
}

async fn connect_iam(i: &IamDatabase, pool_cfg: &PoolConfig) -> anyhow::Result<PgPool> {
    info!(
        host = %i.host,
        port = i.port,
        user = %i.user,
        region = %i.aws_region,
        refresh_secs = i.token_refresh_interval.as_secs(),
        "Ferra: IAM auth enabled; loading AWS SDK config and minting initial RDS auth token",
    );

    let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(i.aws_region.clone()))
        .load()
        .await;

    let initial_opts = build_iam_options(i, &sdk_cfg).await?;
    // Pool sizing and timeouts come from PoolConfig (env-tunable). The IAM
    // safety property — that connections rotate while their auth token is
    // still valid for new connections — is enforced in config validation,
    // which rejects max_lifetime=0 or >= 15 min when IAM is enabled.
    let pool = apply_pool_config(PgPoolOptions::new(), pool_cfg)
        .connect_with(initial_opts)
        .await?;

    // Background refresher: regenerates the IAM token periodically and
    // updates the pool's connect_options. Existing connections continue
    // using their original token (which is fine — they were authenticated
    // at connect time and stay authenticated for their lifetime). New
    // connections — i.e. when the pool grows or when max_lifetime rotates
    // a connection out — use the freshest token.
    let pool_for_task = pool.clone();
    let cfg_for_task = i.clone();
    let sdk_for_task = sdk_cfg.clone();
    let interval = i.token_refresh_interval;
    tokio::spawn(async move {
        run_token_refresher(pool_for_task, cfg_for_task, sdk_for_task, interval).await;
    });

    Ok(pool)
}

async fn build_iam_options(
    cfg: &IamDatabase,
    sdk_cfg: &aws_config::SdkConfig,
) -> anyhow::Result<PgConnectOptions> {
    use aws_sdk_rds::auth_token::{AuthTokenGenerator, Config as AuthTokenConfig};

    let token_cfg = AuthTokenConfig::builder()
        .hostname(&cfg.host)
        // AuthTokenConfig::port wants u64; PgConnectOptions::port wants u16.
        .port(u64::from(cfg.port))
        .region(aws_config::Region::new(cfg.aws_region.clone()))
        .username(&cfg.user)
        .build()
        .map_err(|e| anyhow::anyhow!("build RDS auth-token config: {e}"))?;

    let generator = AuthTokenGenerator::new(token_cfg);
    let token = generator
        .auth_token(sdk_cfg)
        .await
        .map_err(|e| anyhow::anyhow!("mint RDS auth token: {e}"))?;

    let mut opts = PgConnectOptions::new()
        .host(&cfg.host)
        .port(cfg.port)
        .database(&cfg.name)
        .username(&cfg.user)
        .password(token.as_str())
        .ssl_mode(cfg.ssl_mode);
    if let Some(ca) = cfg.ssl_root_cert.as_ref() {
        opts = opts.ssl_root_cert(ca);
    }
    Ok(opts)
}

async fn run_token_refresher(
    pool: PgPool,
    cfg: IamDatabase,
    sdk_cfg: aws_config::SdkConfig,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately; skip it because we already minted a
    // token before spawning this task.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        match build_iam_options(&cfg, &sdk_cfg).await {
            Ok(new_opts) => {
                // set_connect_options only affects FUTURE connections.
                // Existing connections continue using their original
                // password (token), which is exactly what we want.
                pool.set_connect_options(new_opts);
                info!(
                    host = %cfg.host,
                    "Ferra: IAM token refreshed; pool will use the new token for new connections",
                );
            }
            Err(e) => {
                // Don't panic, don't close the pool. The previous
                // connect_options stays in effect; existing connections
                // keep working. We retry on the next tick.
                error!(
                    host = %cfg.host,
                    error = %e,
                    "Ferra: IAM token refresh failed; keeping previous token, will retry next tick",
                );
            }
        }
    }
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn escape_like_passes_safe_chars() {
        assert_eq!(escape_like(""), "");
        assert_eq!(escape_like("services/payment/"), "services/payment/");
        assert_eq!(escape_like("abc-xyz"), "abc-xyz");
        assert_eq!(escape_like("with spaces and 1234"), "with spaces and 1234");
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c\\d"), "c\\\\d");
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
    }
}
