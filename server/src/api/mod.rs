pub mod events;
pub mod health;
pub mod kv;
pub mod watch;

use axum::{routing::get, Router};
use tower_http::trace::TraceLayer;

use crate::state::SharedState;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route(
            "/v1/kv/*key",
            get(kv::get_key).put(kv::set_key).delete(kv::delete_key),
        )
        .route("/v1/kv", get(kv::list_prefix))
        .route("/v1/events", get(events::list_events))
        .route("/v1/watch", get(watch::watch))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
