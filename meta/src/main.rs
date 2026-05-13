//! Placeholder binary for the `ferra` crate name.
//!
//! Ferra is split into two real crates:
//! - `ferra-server`: the HTTP + SSE configuration server
//! - `ferra-agent`: the sidecar that holds an in-memory cache and exposes
//!   a localhost HTTP API to nearby service containers
//!
//! This binary just prints redirection instructions and exits.

fn main() {
    println!("Ferra: a lightweight Postgres-backed configuration center.");
    println!();
    println!("This `ferra` binary is a placeholder. The real functionality lives in:");
    println!("  ferra-server  — the HTTP + SSE configuration server");
    println!("  ferra-agent   — the sidecar (in-memory cache + localhost HTTP)");
    println!();
    println!("Install whichever you need:");
    println!("  cargo install ferra-server");
    println!("  cargo install ferra-agent");
    println!();
    println!("Source: https://github.com/deatherving/ferra");
}
