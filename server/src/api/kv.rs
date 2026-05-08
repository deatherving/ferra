use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    db::escape_like,
    error::{AppError, AppResult},
    events::{ChangeEvent, Operation},
    state::SharedState,
};

#[derive(Deserialize)]
pub struct SetBody {
    pub value: serde_json::Value,
}

#[derive(Serialize)]
pub struct WriteResponse {
    pub key: String,
    pub event_id: i64,
    pub operation: &'static str,
}

#[derive(Serialize)]
pub struct GetResponse {
    pub key: String,
    pub value: serde_json::Value,
    pub event_id: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct ListItem {
    pub key: String,
    pub value: serde_json::Value,
    pub event_id: i64,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub prefix: String,
    pub latest_event_id: i64,
    pub items: Vec<ListItem>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub prefix: String,
}

fn validate_key(key: &str) -> AppResult<()> {
    if key.is_empty() {
        return Err(AppError::BadRequest("key must not be empty".into()));
    }
    if key.len() > 1024 {
        return Err(AppError::BadRequest("key too long (max 1024 chars)".into()));
    }
    if key.contains('\0') {
        return Err(AppError::BadRequest("key must not contain NUL".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_key;
    use crate::error::AppError;

    #[test]
    fn ok_for_typical_keys() {
        assert!(validate_key("services/payment/timeout_ms").is_ok());
        assert!(validate_key("a").is_ok());
    }

    #[test]
    fn rejects_empty() {
        let e = validate_key("").unwrap_err();
        assert!(matches!(e, AppError::BadRequest(_)));
        assert!(e.to_string().contains("empty"));
    }

    #[test]
    fn rejects_too_long() {
        let key = "a".repeat(1025);
        let e = validate_key(&key).unwrap_err();
        assert!(matches!(e, AppError::BadRequest(_)));
        assert!(e.to_string().contains("too long"));
    }

    #[test]
    fn accepts_max_length() {
        let key = "a".repeat(1024);
        assert!(validate_key(&key).is_ok());
    }

    #[test]
    fn rejects_nul_byte() {
        let e = validate_key("foo\0bar").unwrap_err();
        assert!(matches!(e, AppError::BadRequest(_)));
        assert!(e.to_string().contains("NUL"));
    }
}

pub async fn get_key(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> AppResult<Json<GetResponse>> {
    validate_key(&key)?;
    let row = sqlx::query(
        "SELECT key, value, event_id, updated_at FROM kv_configs WHERE key = $1",
    )
    .bind(&key)
    .fetch_optional(&state.pool)
    .await?;
    let row = row.ok_or(AppError::NotFound)?;
    Ok(Json(GetResponse {
        key: row.try_get("key")?,
        value: row.try_get("value")?,
        event_id: row.try_get("event_id")?,
        updated_at: row.try_get("updated_at")?,
    }))
}

pub async fn set_key(
    State(state): State<SharedState>,
    Path(key): Path<String>,
    Json(body): Json<SetBody>,
) -> AppResult<Json<WriteResponse>> {
    validate_key(&key)?;
    let serialized = serde_json::to_vec(&body.value)?;
    if serialized.len() > state.config.max_value_bytes {
        return Err(AppError::PayloadTooLarge);
    }

    let mut tx = state.pool.begin().await?;
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO kv_events (key, operation, value) VALUES ($1, 'set', $2) RETURNING id",
    )
    .bind(&key)
    .bind(&body.value)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO kv_configs (key, value, event_id, updated_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (key) DO UPDATE \
         SET value = EXCLUDED.value, event_id = EXCLUDED.event_id, updated_at = now()",
    )
    .bind(&key)
    .bind(&body.value)
    .bind(event_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let _ = state.events_tx.send(ChangeEvent {
        event_id,
        key: key.clone(),
        operation: Operation::Set,
    });

    Ok(Json(WriteResponse {
        key,
        event_id,
        operation: "set",
    }))
}

pub async fn delete_key(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> AppResult<impl IntoResponse> {
    validate_key(&key)?;

    let mut tx = state.pool.begin().await?;
    let affected = sqlx::query("DELETE FROM kv_configs WHERE key = $1")
        .bind(&key)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if affected == 0 {
        tx.rollback().await?;
        return Err(AppError::NotFound);
    }
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO kv_events (key, operation, value) VALUES ($1, 'delete', NULL) RETURNING id",
    )
    .bind(&key)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let _ = state.events_tx.send(ChangeEvent {
        event_id,
        key: key.clone(),
        operation: Operation::Delete,
    });

    Ok((
        StatusCode::OK,
        Json(WriteResponse {
            key,
            event_id,
            operation: "delete",
        }),
    ))
}

pub async fn list_prefix(
    State(state): State<SharedState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<ListResponse>> {
    if q.prefix.len() > 1024 {
        return Err(AppError::BadRequest("prefix too long".into()));
    }
    let prefix_pattern = format!("{}%", escape_like(&q.prefix));
    let rows = sqlx::query(
        "SELECT key, value, event_id FROM kv_configs \
         WHERE key LIKE $1 ESCAPE '\\' ORDER BY key",
    )
    .bind(&prefix_pattern)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<ListItem> = rows
        .into_iter()
        .map(|r| {
            Ok::<_, sqlx::Error>(ListItem {
                key: r.try_get("key")?,
                value: r.try_get("value")?,
                event_id: r.try_get("event_id")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?;
    let latest_event_id: Option<i64> =
        sqlx::query_scalar("SELECT MAX(id) FROM kv_events")
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(ListResponse {
        prefix: q.prefix,
        latest_event_id: latest_event_id.unwrap_or(0),
        items,
    }))
}
