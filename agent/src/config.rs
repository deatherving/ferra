use std::time::Duration;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "ferra-agent", version, about = "Ferra sidecar agent")]
pub struct Args {
    /// Ferra server endpoint, e.g. http://ferra-server.ferra.svc.cluster.local:8080
    #[arg(long, env = "FERRA_SERVER")]
    pub server: String,

    /// Prefix(es) to watch. Can be repeated; or pass a comma-separated list
    /// via FERRA_PREFIX. The agent loads a snapshot and watches each prefix
    /// independently; service requests for keys not under any of these
    /// prefixes return 404.
    #[arg(long, env = "FERRA_PREFIX", value_delimiter = ',', num_args = 0..)]
    pub prefix: Vec<String>,

    /// Local listen address for the agent's HTTP API.
    #[arg(long, env = "FERRA_AGENT_LISTEN", default_value = "127.0.0.1:9999")]
    pub listen: String,

    /// Minimum reconnect backoff after a watch transport error.
    #[arg(long, env = "FERRA_MIN_BACKOFF", default_value = "500ms",
          value_parser = parse_dur)]
    pub min_backoff: Duration,

    /// Maximum reconnect backoff (ceiling for exponential growth).
    #[arg(long, env = "FERRA_MAX_BACKOFF", default_value = "30s",
          value_parser = parse_dur)]
    pub max_backoff: Duration,
}

fn parse_dur(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| format!("invalid duration {s:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::Parser;
    use std::time::Duration;

    #[test]
    fn parses_required_args() {
        let a = Args::try_parse_from([
            "ferra-agent",
            "--server",
            "http://x:8080",
            "--prefix",
            "services/payment/",
        ])
        .unwrap();
        assert_eq!(a.server, "http://x:8080");
        assert_eq!(a.prefix, vec!["services/payment/"]);
        assert_eq!(a.listen, "127.0.0.1:9999");
        assert_eq!(a.min_backoff, Duration::from_millis(500));
        assert_eq!(a.max_backoff, Duration::from_secs(30));
    }

    #[test]
    fn parses_multiple_prefixes() {
        let a = Args::try_parse_from([
            "ferra-agent",
            "--server",
            "http://x:8080",
            "--prefix",
            "services/payment/",
            "--prefix",
            "flags/global/",
        ])
        .unwrap();
        assert_eq!(a.prefix, vec!["services/payment/", "flags/global/"]);
    }

    #[test]
    fn parses_durations() {
        let a = Args::try_parse_from([
            "ferra-agent",
            "--server",
            "http://x:8080",
            "--prefix",
            "p/",
            "--min-backoff",
            "1s",
            "--max-backoff",
            "1m",
        ])
        .unwrap();
        assert_eq!(a.min_backoff, Duration::from_secs(1));
        assert_eq!(a.max_backoff, Duration::from_secs(60));
    }
}
