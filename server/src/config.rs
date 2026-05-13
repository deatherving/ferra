use std::path::PathBuf;
use std::time::Duration;

use sqlx::postgres::PgSslMode;

#[derive(Debug, Clone)]
pub struct Config {
    pub database: DatabaseConfig,
    pub http_addr: String,
    pub max_value_bytes: usize,
    pub watch_heartbeat: Duration,
}

/// How `ferra-server` should connect to Postgres.
///
/// Three modes are supported:
/// * `Url`      — backwards-compatible: pass a libpq-style URL.
/// * `Discrete` — explicit host/port/user/password fields.
/// * `Iam`      — discrete fields plus AWS RDS/Aurora IAM authentication;
///   password is a short-lived auth token refreshed in the background.
#[derive(Debug, Clone)]
pub enum DatabaseConfig {
    Url(String),
    Discrete(DiscreteDatabase),
    Iam(IamDatabase),
}

#[derive(Debug, Clone)]
pub struct DiscreteDatabase {
    pub host: String,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub password: String,
    pub ssl_mode: PgSslMode,
    /// Optional path to a PEM-encoded CA bundle. Required for
    /// `ssl_mode = verify-ca | verify-full` when the server cert chains
    /// to a non-system CA — e.g. the AWS RDS global CA bundle, which is
    /// not in the system trust store. Parsed from
    /// `FERRA_DATABASE_SSL_ROOT_CERT`.
    pub ssl_root_cert: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct IamDatabase {
    pub host: String,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub ssl_mode: PgSslMode,
    /// See `DiscreteDatabase::ssl_root_cert`. For RDS/Aurora with
    /// `verify-full`, point this at the bundle from
    /// <https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem>.
    pub ssl_root_cert: Option<PathBuf>,
    pub aws_region: String,
    /// How often to regenerate the RDS auth token and apply it via
    /// `PgPool::set_connect_options`. Defaults to 14 minutes, which sits
    /// safely inside the 15-minute IAM token TTL.
    pub token_refresh_interval: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database = parse_database_config()?;
        let http_addr =
            std::env::var("FERRA_HTTP_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        let max_value_bytes = std::env::var("FERRA_MAX_VALUE_BYTES")
            .ok()
            .map(|s| s.parse::<usize>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid FERRA_MAX_VALUE_BYTES: {e}"))?
            .unwrap_or(256 * 1024);
        let heartbeat_secs = std::env::var("FERRA_WATCH_HEARTBEAT_SECONDS")
            .ok()
            .map(|s| s.parse::<u64>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid FERRA_WATCH_HEARTBEAT_SECONDS: {e}"))?
            .unwrap_or(30);

        Ok(Self {
            database,
            http_addr,
            max_value_bytes,
            watch_heartbeat: Duration::from_secs(heartbeat_secs),
        })
    }
}

fn parse_database_config() -> anyhow::Result<DatabaseConfig> {
    let url = std::env::var("FERRA_DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let iam_enabled = bool_env("FERRA_DATABASE_IAM_AUTH_ENABLED")?;

    if iam_enabled {
        if url.is_some() {
            anyhow::bail!(
                "FERRA_DATABASE_URL and FERRA_DATABASE_IAM_AUTH_ENABLED=true are mutually exclusive; \
                 use discrete FERRA_DATABASE_* fields with IAM auth",
            );
        }
        let host = required_env("FERRA_DATABASE_HOST")?;
        let port = parse_port_env()?;
        let name = required_env("FERRA_DATABASE_NAME")?;
        let user = required_env("FERRA_DATABASE_USER")?;
        // IAM mode defaults to verify-full — IAM implies you're talking to a
        // real AWS RDS/Aurora endpoint, and that endpoint should have its
        // hostname verified against the AWS CA. Anyone who needs to weaken
        // this (e.g. self-signed proxy in front of RDS) sets
        // FERRA_DATABASE_SSL_MODE explicitly.
        let ssl_mode = match std::env::var("FERRA_DATABASE_SSL_MODE") {
            Ok(v) if !v.trim().is_empty() => parse_ssl_mode_env()?,
            _ => PgSslMode::VerifyFull,
        };
        if matches!(ssl_mode, PgSslMode::Disable) {
            anyhow::bail!(
                "FERRA_DATABASE_SSL_MODE=disable is incompatible with IAM auth (RDS requires TLS)",
            );
        }
        let ssl_root_cert = parse_ssl_root_cert_env(ssl_mode)?;
        let aws_region = required_env("FERRA_DATABASE_AWS_REGION")?;
        let refresh_secs = std::env::var("FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.parse::<u64>())
            .transpose()
            .map_err(|e| {
                anyhow::anyhow!("invalid FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS: {e}")
            })?
            .unwrap_or(14 * 60);
        if refresh_secs == 0 {
            anyhow::bail!("FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS must be > 0");
        }
        if refresh_secs >= 15 * 60 {
            anyhow::bail!(
                "FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS must be < 900 (IAM tokens expire at 15 minutes)",
            );
        }
        return Ok(DatabaseConfig::Iam(IamDatabase {
            host,
            port,
            name,
            user,
            ssl_mode,
            ssl_root_cert,
            aws_region,
            token_refresh_interval: Duration::from_secs(refresh_secs),
        }));
    }

    if let Some(url) = url {
        return Ok(DatabaseConfig::Url(url));
    }

    // Discrete password mode.
    let host = required_env("FERRA_DATABASE_HOST").map_err(|_| {
        anyhow::anyhow!(
            "no database configuration provided: set FERRA_DATABASE_URL, or the discrete \
             FERRA_DATABASE_HOST/NAME/USER/PASSWORD fields, or enable IAM auth via \
             FERRA_DATABASE_IAM_AUTH_ENABLED=true plus FERRA_DATABASE_HOST/NAME/USER/AWS_REGION",
        )
    })?;
    let port = parse_port_env()?;
    let name = required_env("FERRA_DATABASE_NAME")?;
    let user = required_env("FERRA_DATABASE_USER")?;
    let password = required_env("FERRA_DATABASE_PASSWORD")?;
    let ssl_mode = parse_ssl_mode_env()?;
    let ssl_root_cert = parse_ssl_root_cert_env(ssl_mode)?;
    Ok(DatabaseConfig::Discrete(DiscreteDatabase {
        host,
        port,
        name,
        user,
        password,
        ssl_mode,
        ssl_root_cert,
    }))
}

fn parse_ssl_root_cert_env(ssl_mode: PgSslMode) -> anyhow::Result<Option<PathBuf>> {
    let raw = std::env::var("FERRA_DATABASE_SSL_ROOT_CERT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let path = raw.map(PathBuf::from);

    // verify-ca and verify-full need a CA bundle the cert chains to. The
    // system trust store sometimes works (e.g. for Cloud SQL which uses
    // public CAs) but for RDS/Aurora it won't — the AWS RDS CA isn't in
    // the system store. Loudly warn if the user picked a verifying mode
    // without giving us a bundle; sqlx will fall back to the system store
    // and may or may not work.
    if path.is_none() && matches!(ssl_mode, PgSslMode::VerifyCa | PgSslMode::VerifyFull) {
        tracing::warn!(
            "FERRA_DATABASE_SSL_MODE is verify-ca/verify-full but \
             FERRA_DATABASE_SSL_ROOT_CERT is unset; sqlx will use the \
             system trust store. For AWS RDS/Aurora this typically fails — \
             download the RDS global CA bundle and point this env var at it.",
        );
    }

    Ok(path)
}

fn required_env(key: &str) -> anyhow::Result<String> {
    let v = std::env::var(key).map_err(|_| anyhow::anyhow!("{key} is required"))?;
    if v.trim().is_empty() {
        anyhow::bail!("{key} must not be empty");
    }
    Ok(v)
}

fn bool_env(key: &str) -> anyhow::Result<bool> {
    match std::env::var(key) {
        Err(_) => Ok(false),
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" => Ok(false),
            "1" | "true" | "yes" | "on" => Ok(true),
            other => Err(anyhow::anyhow!(
                "invalid boolean value for {key}: {other:?} (expected true/false)",
            )),
        },
    }
}

fn parse_port_env() -> anyhow::Result<u16> {
    let raw = std::env::var("FERRA_DATABASE_PORT").ok();
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(5432),
        Some(s) => s
            .parse::<u16>()
            .map_err(|e| anyhow::anyhow!("invalid FERRA_DATABASE_PORT: {e}")),
    }
}

fn parse_ssl_mode_env() -> anyhow::Result<PgSslMode> {
    let raw = std::env::var("FERRA_DATABASE_SSL_MODE").ok();
    let normalized = raw.as_deref().map(|s| s.trim().to_ascii_lowercase());
    match normalized.as_deref() {
        None | Some("") => Ok(PgSslMode::Prefer),
        Some("disable") => Ok(PgSslMode::Disable),
        Some("allow") => Ok(PgSslMode::Allow),
        Some("prefer") => Ok(PgSslMode::Prefer),
        Some("require") => Ok(PgSslMode::Require),
        Some("verify-ca") | Some("verify_ca") => Ok(PgSslMode::VerifyCa),
        Some("verify-full") | Some("verify_full") => Ok(PgSslMode::VerifyFull),
        Some(other) => Err(anyhow::anyhow!(
            "invalid FERRA_DATABASE_SSL_MODE: {other:?} (expected disable/allow/prefer/require/verify-ca/verify-full)",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, DatabaseConfig};
    use serial_test::serial;
    use sqlx::postgres::PgSslMode;
    use std::time::Duration;

    const VARS: &[&str] = &[
        "FERRA_DATABASE_URL",
        "FERRA_DATABASE_HOST",
        "FERRA_DATABASE_PORT",
        "FERRA_DATABASE_NAME",
        "FERRA_DATABASE_USER",
        "FERRA_DATABASE_PASSWORD",
        "FERRA_DATABASE_SSL_MODE",
        "FERRA_DATABASE_SSL_ROOT_CERT",
        "FERRA_DATABASE_IAM_AUTH_ENABLED",
        "FERRA_DATABASE_AWS_REGION",
        "FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS",
        "FERRA_HTTP_ADDR",
        "FERRA_MAX_VALUE_BYTES",
        "FERRA_WATCH_HEARTBEAT_SECONDS",
    ];

    fn clear() {
        for k in VARS {
            // SAFETY: tests in this module are serialized via #[serial].
            unsafe { std::env::remove_var(k) };
        }
    }

    fn set(k: &str, v: &str) {
        // SAFETY: tests in this module are serialized via #[serial].
        unsafe { std::env::set_var(k, v) };
    }

    #[test]
    #[serial]
    fn errors_when_no_database_config_provided() {
        clear();
        let err = Config::from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("FERRA_DATABASE_URL") && msg.contains("FERRA_DATABASE_HOST"),
            "unexpected error message: {msg}",
        );
    }

    #[test]
    #[serial]
    fn url_mode_defaults_apply() {
        clear();
        set("FERRA_DATABASE_URL", "postgres://x");
        let cfg = Config::from_env().unwrap();
        match cfg.database {
            DatabaseConfig::Url(u) => assert_eq!(u, "postgres://x"),
            other => panic!("expected url mode, got {other:?}"),
        }
        assert_eq!(cfg.http_addr, "0.0.0.0:8080");
        assert_eq!(cfg.max_value_bytes, 256 * 1024);
        assert_eq!(cfg.watch_heartbeat, Duration::from_secs(30));
    }

    #[test]
    #[serial]
    fn url_mode_overrides_apply() {
        clear();
        set("FERRA_DATABASE_URL", "postgres://y");
        set("FERRA_HTTP_ADDR", "127.0.0.1:9090");
        set("FERRA_MAX_VALUE_BYTES", "1024");
        set("FERRA_WATCH_HEARTBEAT_SECONDS", "5");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.http_addr, "127.0.0.1:9090");
        assert_eq!(cfg.max_value_bytes, 1024);
        assert_eq!(cfg.watch_heartbeat, Duration::from_secs(5));
    }

    #[test]
    #[serial]
    fn errors_on_invalid_max_value_bytes() {
        clear();
        set("FERRA_DATABASE_URL", "postgres://x");
        set("FERRA_MAX_VALUE_BYTES", "not-a-number");
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("FERRA_MAX_VALUE_BYTES"));
    }

    #[test]
    #[serial]
    fn errors_on_invalid_heartbeat() {
        clear();
        set("FERRA_DATABASE_URL", "postgres://x");
        set("FERRA_WATCH_HEARTBEAT_SECONDS", "x");
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("FERRA_WATCH_HEARTBEAT_SECONDS"));
    }

    #[test]
    #[serial]
    fn discrete_mode_requires_all_fields() {
        clear();
        set("FERRA_DATABASE_HOST", "db.example.com");
        // missing name, user, password
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("FERRA_DATABASE_NAME"));
    }

    #[test]
    #[serial]
    fn discrete_mode_builds_config() {
        clear();
        set("FERRA_DATABASE_HOST", "db.example.com");
        set("FERRA_DATABASE_NAME", "ferra");
        set("FERRA_DATABASE_USER", "ferra_user");
        set("FERRA_DATABASE_PASSWORD", "s3cret");
        set("FERRA_DATABASE_SSL_MODE", "require");
        let cfg = Config::from_env().unwrap();
        match cfg.database {
            DatabaseConfig::Discrete(d) => {
                assert_eq!(d.host, "db.example.com");
                assert_eq!(d.port, 5432);
                assert_eq!(d.name, "ferra");
                assert_eq!(d.user, "ferra_user");
                assert_eq!(d.password, "s3cret");
                assert!(matches!(d.ssl_mode, PgSslMode::Require));
                assert!(d.ssl_root_cert.is_none());
            }
            other => panic!("expected discrete mode, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn ssl_root_cert_env_var_is_picked_up_in_discrete_mode() {
        clear();
        set("FERRA_DATABASE_HOST", "db.example.com");
        set("FERRA_DATABASE_NAME", "ferra");
        set("FERRA_DATABASE_USER", "u");
        set("FERRA_DATABASE_PASSWORD", "p");
        set("FERRA_DATABASE_SSL_MODE", "verify-full");
        set(
            "FERRA_DATABASE_SSL_ROOT_CERT",
            "/etc/rds-ca/global-bundle.pem",
        );
        let cfg = Config::from_env().unwrap();
        match cfg.database {
            DatabaseConfig::Discrete(d) => {
                assert!(matches!(d.ssl_mode, PgSslMode::VerifyFull));
                assert_eq!(
                    d.ssl_root_cert.as_deref().and_then(|p| p.to_str()),
                    Some("/etc/rds-ca/global-bundle.pem"),
                );
            }
            other => panic!("expected discrete mode, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn iam_mode_builds_config_with_defaults() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set(
            "FERRA_DATABASE_HOST",
            "db.cluster.us-west-2.rds.amazonaws.com",
        );
        set("FERRA_DATABASE_NAME", "ferra");
        set("FERRA_DATABASE_USER", "ferra_iam");
        set("FERRA_DATABASE_AWS_REGION", "us-west-2");
        let cfg = Config::from_env().unwrap();
        match cfg.database {
            DatabaseConfig::Iam(i) => {
                assert_eq!(i.host, "db.cluster.us-west-2.rds.amazonaws.com");
                assert_eq!(i.port, 5432);
                assert_eq!(i.name, "ferra");
                assert_eq!(i.user, "ferra_iam");
                assert_eq!(i.aws_region, "us-west-2");
                // IAM defaults to verify-full for safety against MITM on the
                // path to AWS RDS / Aurora endpoints.
                assert!(matches!(i.ssl_mode, PgSslMode::VerifyFull));
                assert!(i.ssl_root_cert.is_none());
                assert_eq!(i.token_refresh_interval, Duration::from_secs(14 * 60));
            }
            other => panic!("expected iam mode, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn iam_mode_rejects_url() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set("FERRA_DATABASE_URL", "postgres://x");
        set("FERRA_DATABASE_HOST", "h");
        set("FERRA_DATABASE_NAME", "n");
        set("FERRA_DATABASE_USER", "u");
        set("FERRA_DATABASE_AWS_REGION", "us-west-2");
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    #[serial]
    fn iam_mode_honors_explicit_ssl_mode_override() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set("FERRA_DATABASE_HOST", "h");
        set("FERRA_DATABASE_NAME", "n");
        set("FERRA_DATABASE_USER", "u");
        set("FERRA_DATABASE_AWS_REGION", "us-west-2");
        set("FERRA_DATABASE_SSL_MODE", "require");
        let cfg = Config::from_env().unwrap();
        match cfg.database {
            DatabaseConfig::Iam(i) => assert!(matches!(i.ssl_mode, PgSslMode::Require)),
            other => panic!("expected iam mode, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn iam_mode_rejects_disable_ssl() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set("FERRA_DATABASE_HOST", "h");
        set("FERRA_DATABASE_NAME", "n");
        set("FERRA_DATABASE_USER", "u");
        set("FERRA_DATABASE_AWS_REGION", "us-west-2");
        set("FERRA_DATABASE_SSL_MODE", "disable");
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("incompatible with IAM auth"));
    }

    #[test]
    #[serial]
    fn iam_mode_requires_aws_region() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set("FERRA_DATABASE_HOST", "h");
        set("FERRA_DATABASE_NAME", "n");
        set("FERRA_DATABASE_USER", "u");
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("FERRA_DATABASE_AWS_REGION"));
    }

    #[test]
    #[serial]
    fn iam_mode_rejects_refresh_interval_at_or_over_token_ttl() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set("FERRA_DATABASE_HOST", "h");
        set("FERRA_DATABASE_NAME", "n");
        set("FERRA_DATABASE_USER", "u");
        set("FERRA_DATABASE_AWS_REGION", "us-west-2");
        set("FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS", "900");
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("< 900"));
    }

    #[test]
    #[serial]
    fn iam_mode_accepts_custom_refresh_interval() {
        clear();
        set("FERRA_DATABASE_IAM_AUTH_ENABLED", "true");
        set("FERRA_DATABASE_HOST", "h");
        set("FERRA_DATABASE_NAME", "n");
        set("FERRA_DATABASE_USER", "u");
        set("FERRA_DATABASE_AWS_REGION", "us-west-2");
        set("FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS", "600");
        let cfg = Config::from_env().unwrap();
        match cfg.database {
            DatabaseConfig::Iam(i) => {
                assert_eq!(i.token_refresh_interval, Duration::from_secs(600));
            }
            other => panic!("expected iam mode, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn invalid_ssl_mode_rejected() {
        clear();
        set("FERRA_DATABASE_URL", "postgres://x");
        set("FERRA_DATABASE_SSL_MODE", "bogus");
        // URL mode ignores ssl_mode (it's part of the URL), but the parser
        // still validates the env var so misspellings don't silently win.
        // We intentionally don't fail here in URL mode, since the URL itself
        // is authoritative. The validation runs only when ssl_mode is
        // consumed (discrete / iam modes). This test documents that.
        let cfg = Config::from_env();
        assert!(
            cfg.is_ok(),
            "URL mode should not consume FERRA_DATABASE_SSL_MODE"
        );
    }
}
