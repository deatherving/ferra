use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub http_addr: String,
    pub max_value_bytes: usize,
    pub watch_heartbeat: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("FERRA_DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("FERRA_DATABASE_URL is required"))?;
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
            database_url,
            http_addr,
            max_value_bytes,
            watch_heartbeat: Duration::from_secs(heartbeat_secs),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    use serial_test::serial;
    use std::time::Duration;

    const VARS: &[&str] = &[
        "FERRA_DATABASE_URL",
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
    fn errors_when_database_url_missing() {
        clear();
        let err = Config::from_env().unwrap_err();
        assert!(err.to_string().contains("FERRA_DATABASE_URL"));
    }

    #[test]
    #[serial]
    fn defaults_apply() {
        clear();
        set("FERRA_DATABASE_URL", "postgres://x");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.database_url, "postgres://x");
        assert_eq!(cfg.http_addr, "0.0.0.0:8080");
        assert_eq!(cfg.max_value_bytes, 256 * 1024);
        assert_eq!(cfg.watch_heartbeat, Duration::from_secs(30));
    }

    #[test]
    #[serial]
    fn overrides_apply() {
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
}
