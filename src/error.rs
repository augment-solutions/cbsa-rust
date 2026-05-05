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

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
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

/// Generic detail emitted for every 5xx response. The wire body intentionally
/// withholds underlying error text so that sqlx fragments, abend messages
/// constructed with embedded `Display` of an upstream error, query strings,
/// or other internal details never reach the caller. The full error is
/// recorded server-side via `tracing::error!`.
const GENERIC_FAILURE_DETAIL: &str =
    "Operation failed. Please contact support if the problem persists.";

impl IntoResponse for CbsaError {
    fn into_response(self) -> Response {
        // The full error (including any underlying sqlx::Error and free-form
        // abend message string) is recorded server-side only. The wire body
        // never carries the variant's `Display` output.
        tracing::error!(error = ?self, "cbsa error");
        let (status, pd) = match &self {
            CbsaError::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                ProblemDetail::new(StatusCode::BAD_REQUEST, "Validation failed", msg.clone()),
            ),
            CbsaError::Abend(code, _) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ProblemDetail::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Abend",
                    GENERIC_FAILURE_DETAIL,
                )
                .with_abend_code(*code),
            ),
            CbsaError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ProblemDetail::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unexpected error",
                    GENERIC_FAILURE_DETAIL,
                )
                .with_abend_code(UNEXPECTED_ABEND_CODE),
            ),
        };
        problem_response(status, &pd)
    }
}

/// Build a `Response` whose body is the JSON serialisation of `pd` and whose
/// `Content-Type` is `application/problem+json` per RFC 7807 §3. Using the
/// dedicated media type lets clients dispatch on the response shape without
/// sniffing the body.
pub fn problem_response(status: StatusCode, pd: &ProblemDetail) -> Response {
    let body = serde_json::to_vec(pd).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    response
}
