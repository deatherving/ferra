use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            AppError::Sqlx(_) | AppError::Json(_) | AppError::Internal(_) => {
                tracing::error!(error = %self, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        let body = Json(json!({
            "error": code,
            "message": self.to_string(),
        }));
        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::AppError;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    async fn body_of(err: AppError) -> (StatusCode, String) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn not_found_maps_to_404() {
        let (status, body) = body_of(AppError::NotFound).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("\"error\":\"not_found\""));
    }

    #[tokio::test]
    async fn bad_request_maps_to_400() {
        let (status, body) = body_of(AppError::BadRequest("oops".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("\"error\":\"bad_request\""));
        assert!(body.contains("oops"));
    }

    #[tokio::test]
    async fn payload_too_large_maps_to_413() {
        let (status, _) = body_of(AppError::PayloadTooLarge).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn sqlx_error_maps_to_500() {
        let err = AppError::Sqlx(sqlx::Error::PoolClosed);
        let (status, body) = body_of(err).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("\"error\":\"internal_error\""));
    }

    #[tokio::test]
    async fn internal_maps_to_500() {
        let (status, _) = body_of(AppError::Internal("boom".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn json_error_maps_to_500() {
        let err: serde_json::Error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let (status, _) = body_of(AppError::Json(err)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
