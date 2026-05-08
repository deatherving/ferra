use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    db::escape_like,
    error::{AppError, AppResult},
    state::SharedState,
};

#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub since: i64,
    pub prefix: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    1000
}

#[derive(Serialize)]
pub struct EventItem {
    pub event_id: i64,
    pub key: String,
    pub operation: String,
}

#[derive(Serialize)]
pub struct EventsResponse {
    pub from_event_id: i64,
    pub to_event_id: i64,
    pub events: Vec<EventItem>,
}

pub async fn list_events(
    State(state): State<SharedState>,
    Query(q): Query<EventsQuery>,
) -> AppResult<Json<EventsResponse>> {
    if q.since < 0 {
        return Err(AppError::BadRequest("since must be >= 0".into()));
    }
    let limit = q.limit.clamp(1, 5000);

    let rows = if let Some(prefix) = q.prefix.as_deref() {
        let pat = format!("{}%", escape_like(prefix));
        sqlx::query(
            "SELECT id, key, operation FROM kv_events \
             WHERE id > $1 AND key LIKE $2 ESCAPE '\\' \
             ORDER BY id ASC LIMIT $3",
        )
        .bind(q.since)
        .bind(&pat)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, key, operation FROM kv_events \
             WHERE id > $1 \
             ORDER BY id ASC LIMIT $2",
        )
        .bind(q.since)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    };

    let events: Vec<EventItem> = rows
        .into_iter()
        .map(|r| {
            Ok::<_, sqlx::Error>(EventItem {
                event_id: r.try_get("id")?,
                key: r.try_get("key")?,
                operation: r.try_get("operation")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?;

    let to = events.last().map(|e| e.event_id).unwrap_or(q.since);

    Ok(Json(EventsResponse {
        from_event_id: q.since,
        to_event_id: to,
        events,
    }))
}
