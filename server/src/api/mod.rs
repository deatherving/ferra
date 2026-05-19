pub mod events;
pub mod health;
pub mod kv;
pub mod watch;

use axum::{routing::get, Router};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnRequest, DefaultOnResponse, MakeSpan, TraceLayer},
};
use tracing::{Level, Span};

use crate::state::SharedState;

#[derive(Clone, Copy, Debug)]
struct RequestIdSpan;

impl<B> MakeSpan<B> for RequestIdSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> Span {
        let request_id = request
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        tracing::info_span!(
            "request",
            method = %request.method(),
            uri = %request.uri(),
            request_id = %request_id,
        )
    }
}

pub fn router(state: SharedState) -> Router {
    // Health endpoints are deliberately not wrapped in TraceLayer or
    // request-id middleware: k8s liveness/readiness probes hit them every
    // few seconds and per-request access logs would drown the signal.
    let health = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .with_state(state.clone());

    // In axum, the LAST `.layer` applied is the OUTERMOST wrapper. So
    // SetRequestIdLayer below runs first (assigning x-request-id if the
    // client didn't supply one), then TraceLayer makes a span that captures
    // it, then PropagateRequestIdLayer copies the id onto the response.
    let api = Router::new()
        .route(
            "/v1/kv/*key",
            get(kv::get_key).put(kv::set_key).delete(kv::delete_key),
        )
        .route("/v1/kv", get(kv::list_prefix))
        .route("/v1/events", get(events::list_events))
        .route("/v1/watch", get(watch::watch))
        .with_state(state)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(RequestIdSpan)
                .on_request(DefaultOnRequest::new().level(Level::DEBUG))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

    Router::new().merge(health).merge(api)
}
