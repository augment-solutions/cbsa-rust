//! Cross-cutting error model.
//!
//! - `CbsaError` is the in-process error type. Programs throw a variant; the
//!   axum `IntoResponse` impl maps it to an RFC 7807 `ProblemDetail` body.
//! - `Abend` carries the four-character abend code copied from the COBOL
//!   source (e.g. `CVR1` for INQCUST). PROCTRAN insert failures use the
//!   reserved code `HWPT`; outer retry-exhaustion uses `XRTY`.
//! - "Not found" is **never** an `Err` variant — it is a 404 response built
//!   by the controller from a domain "fail" value. See translation rules §6
//!   and §13.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

pub const PROCTRAN_ABEND_CODE: &str = "HWPT";
pub const RETRY_EXHAUSTED_ABEND_CODE: &str = "XRTY";
pub const UNEXPECTED_ABEND_CODE: &str = "UNEX";

#[derive(Debug, thiserror::Error)]
pub enum CbsaError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("abend {0}: {1}")]
    Abend(&'static str, String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl CbsaError {
    pub fn abend(code: &'static str, message: impl Into<String>) -> Self {
        Self::Abend(code, message.into())
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }
}

#[derive(Debug, Serialize)]
pub struct ProblemDetail {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "abendCode")]
    pub abend_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "failCode")]
    pub fail_code: Option<String>,
}

impl ProblemDetail {
    pub fn new(status: StatusCode, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            problem_type: "about:blank".to_string(),
            title: title.into(),
            status: status.as_u16(),
            detail: detail.into(),
            abend_code: None,
            fail_code: None,
        }
    }

    pub fn with_abend_code(mut self, code: impl Into<String>) -> Self {
        self.abend_code = Some(code.into());
        self
    }

    pub fn with_fail_code(mut self, code: impl Into<String>) -> Self {
        self.fail_code = Some(code.into());
        self
    }
}

impl IntoResponse for CbsaError {
    fn into_response(self) -> Response {
        let (status, pd) = match &self {
            CbsaError::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                ProblemDetail::new(StatusCode::BAD_REQUEST, "Validation failed", msg.clone()),
            ),
            CbsaError::Abend(code, msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ProblemDetail::new(StatusCode::INTERNAL_SERVER_ERROR, "Abend", msg.clone())
                    .with_abend_code(*code),
            ),
            other => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ProblemDetail::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unexpected error",
                    other.to_string(),
                )
                .with_abend_code(UNEXPECTED_ABEND_CODE),
            ),
        };
        tracing::error!(?self, "cbsa error");
        (status, Json(pd)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn problem_detail_serializes_to_rfc7807() {
        let pd = ProblemDetail::new(StatusCode::BAD_REQUEST, "Bad Input", "Field X is invalid")
            .with_fail_code("1");

        let json = serde_json::to_value(&pd).unwrap();

        assert_eq!(json["type"], "about:blank");
        assert_eq!(json["title"], "Bad Input");
        assert_eq!(json["status"], 400);
        assert_eq!(json["detail"], "Field X is invalid");
        assert_eq!(json["failCode"], "1");
        assert!(json.get("abendCode").is_none());
    }

    #[test]
    fn problem_detail_with_abend_code() {
        let pd = ProblemDetail::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "System Error",
            "Database unavailable",
        )
        .with_abend_code("HWPT");

        let json = serde_json::to_value(&pd).unwrap();

        assert_eq!(json["abendCode"], "HWPT");
        assert_eq!(json["status"], 500);
    }

    #[tokio::test]
    async fn cbsa_error_validation_maps_to_400() {
        let err = CbsaError::validation("Missing required field");
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["title"], "Validation failed");
        assert_eq!(json["status"], 400);
        assert_eq!(json["detail"], "Missing required field");
    }

    #[tokio::test]
    async fn cbsa_error_abend_maps_to_500_with_code() {
        let err = CbsaError::abend("CVR1", "Failed to read customer");
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["title"], "Abend");
        assert_eq!(json["status"], 500);
        assert_eq!(json["abendCode"], "CVR1");
        assert_eq!(json["detail"], "Failed to read customer");
    }

    #[tokio::test]
    async fn cbsa_error_database_maps_to_500_with_unex() {
        // Simulate a sqlx error by creating one through parsing
        let db_err: Result<sqlx::postgres::PgConnectOptions, sqlx::Error> =
            "not-a-valid-url".parse();
        let err = CbsaError::from(db_err.unwrap_err());
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["abendCode"], UNEXPECTED_ABEND_CODE);
        assert_eq!(json["status"], 500);
    }
}
