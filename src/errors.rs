//! Central error definitions and HTTP mapping for resolver failures.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, FetchError>;

#[derive(Error, Debug)]
pub enum FetchError {
    #[error("Can't fetch page (Clean)")]
    UnableFetch(#[from] reqwest::Error),
    #[error("Output is not desearializable")]
    UnableToSerialize(#[from] serde_json::Error),
    #[error("Http Failed")]
    Http(String),
    #[error("Upstream fetch failed")]
    Upstream(String),
    #[error("Upstream fetch returned HTTP {status}")]
    UpstreamStatus { status: StatusCode, body: String },
    #[error("Upstream payload reported a failed fetch")]
    UpstreamPayload {
        status: Option<i64>,
        error: Option<String>,
        content: Option<String>,
    },
    #[error("Upstream response exceeded {limit} bytes")]
    UpstreamResponseTooLarge { limit: usize },
    #[error("Fetch timed out")]
    Timeout(String),
    #[error("Not found")]
    NotFound(String),
    #[error("Bad request")]
    BadRequest(String),
    #[error("Payment required")]
    PaymentRequired(String),
    #[error("Credit authorization failed")]
    Credits(String),
    #[error("Idempotency key conflicts with a different request")]
    IdempotencyConflict(String),
    #[error("Egress enforcement unavailable")]
    EgressUnavailable(String),
    #[error("Snapshot failed")]
    Snapshot(#[from] SnapshotError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolverAuthError {
    Unauthorized {
        error: &'static str,
        message: &'static str,
    },
    Forbidden {
        error: &'static str,
        message: &'static str,
    },
    ServiceUnavailable(String),
}

impl ResolverAuthError {
    pub fn challenge_parts(&self) -> Option<(&'static str, &'static str)> {
        match self {
            ResolverAuthError::Unauthorized { error, message }
            | ResolverAuthError::Forbidden { error, message } => Some((error, message)),
            ResolverAuthError::ServiceUnavailable(_) => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ResolverCreditsError {
    #[error("OAuth context is missing Livy {0}")]
    MissingAuth(&'static str),
    #[error("credit request header is invalid: {0}")]
    InvalidHeader(String),
    #[error("credit request failed: {0}")]
    Http(reqwest::Error),
    #[error("credit request returned {status}: {}", compact_body(body))]
    Backend { status: StatusCode, body: String },
    #[error("insufficient user credits: {available} available, {required} required")]
    InsufficientCredits { available: i64, required: i64 },
    #[error("credit capture was not applied: {0}")]
    CaptureNotApplied(String),
    #[error("idempotency key was already bound to a different logical request")]
    IdempotencyConflict,
    #[error("idempotency key was already finalized and no durable prior result is available")]
    IdempotencyAlreadyFinalized,
    #[error("an idempotent debit already exists but no prior resolver result can be replayed")]
    UnsafeReplay,
    #[error("idempotency binding registry failed: {0}")]
    IdempotencyRegistry(String),
}

impl ResolverCreditsError {
    pub fn is_payment_required(&self) -> bool {
        match self {
            Self::InsufficientCredits { .. } => true,
            Self::Backend { status, body } => {
                *status == StatusCode::PAYMENT_REQUIRED
                    || backend_error_code(body).as_deref() == Some("insufficient_user_credits")
            }
            _ => false,
        }
    }

    pub fn is_idempotency_conflict(&self) -> bool {
        match self {
            Self::IdempotencyConflict | Self::IdempotencyAlreadyFinalized | Self::UnsafeReplay => {
                true
            }
            Self::Backend { status, body } => {
                *status == StatusCode::CONFLICT
                    && backend_error_code(body).as_deref() == Some("idempotency_conflict")
            }
            _ => false,
        }
    }

    pub fn is_idempotency_already_finalized(&self) -> bool {
        matches!(self, Self::IdempotencyAlreadyFinalized | Self::UnsafeReplay)
    }
}

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("{0} must be set when provenance is enabled")]
    MissingEnv(&'static str),
    #[error("invalid provenance configuration: {0}")]
    InvalidEnv(String),
    #[error("provenance HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provenance backend returned {status}")]
    Backend { status: StatusCode },
    #[error("provenance response exceeded {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("provenance registry wait failed: {0}")]
    RegistryWait(String),
    #[error("provenance JSON handling failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("provenance attestation failed: {0}")]
    Attestation(String),
    #[error("provenance timestamp failed: {0}")]
    Time(String),
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("Spider snapshot response did not include raw HTML")]
    MissingHtml,
    #[error("Spider snapshot response did not include a screenshot")]
    MissingScreenshot,
}

impl IntoResponse for FetchError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            FetchError::UnableFetch(_)
            | FetchError::Upstream(_)
            | FetchError::UpstreamStatus { .. }
            | FetchError::UpstreamPayload { .. } => (
                StatusCode::BAD_GATEWAY,
                "upstream_fetch_failed",
                "Upstream fetch failed".to_string(),
            ),
            FetchError::UpstreamResponseTooLarge { .. } => (
                StatusCode::BAD_GATEWAY,
                "upstream_response_too_large",
                "Upstream response exceeded the configured limit".to_string(),
            ),
            FetchError::UnableToSerialize(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "response_serialization_failed",
                "Internal response serialization failed".to_string(),
            ),
            FetchError::Http(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal server error".to_string(),
            ),
            FetchError::Timeout(_) => (
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "Upstream request timed out".to_string(),
            ),
            FetchError::NotFound(_) => (
                StatusCode::NOT_FOUND,
                "not_found",
                "Receipt not found".to_string(),
            ),
            FetchError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "invalid_request", message)
            }
            FetchError::PaymentRequired(_) => (
                StatusCode::PAYMENT_REQUIRED,
                "payment_required",
                "Insufficient credits".to_string(),
            ),
            FetchError::Credits(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "credit_service_unavailable",
                "Credit authorization service is unavailable".to_string(),
            ),
            FetchError::IdempotencyConflict(message) => {
                (StatusCode::CONFLICT, "idempotency_conflict", message)
            }
            FetchError::EgressUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "egress_policy_unavailable",
                "Source fetching is unavailable until egress enforcement is ready".to_string(),
            ),
            FetchError::Snapshot(_) => (
                StatusCode::BAD_GATEWAY,
                "invalid_upstream_snapshot",
                "Upstream snapshot response is invalid".to_string(),
            ),
        };

        let body = Json(json!({
            "error": message,
            "code": code,
            "request_id": crate::security::current_request_id(),
        }));
        let mut response = (status, body).into_response();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
}

fn backend_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn compact_body(body: &str) -> String {
    let body = body.trim();
    if body.len() <= 512 {
        return body.to_string();
    }
    let end = (0..=512)
        .rev()
        .find(|index| body.is_char_boundary(*index))
        .unwrap_or(0);
    format!("{}...", &body[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn upstream_errors_are_bad_gateway_and_sanitized() {
        let response = FetchError::Upstream("secret backend detail".into()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json error");
        assert_eq!(value["code"], "upstream_fetch_failed");
        assert_eq!(value["error"], "Upstream fetch failed");
        assert!(!String::from_utf8_lossy(&body).contains("secret backend detail"));

        let response = FetchError::UpstreamStatus {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "secret status body".into(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("status error body");
        assert!(!String::from_utf8_lossy(&body).contains("secret status body"));
    }

    #[tokio::test]
    async fn oversized_upstream_response_has_a_stable_public_code() {
        let response = FetchError::UpstreamResponseTooLarge { limit: 64 }.into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("limit error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json error");
        assert_eq!(value["code"], "upstream_response_too_large");
    }

    #[tokio::test]
    async fn credit_errors_are_service_unavailable() {
        let response = FetchError::Credits("backend response".into()).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(axum::http::header::RETRY_AFTER),
            Some(&axum::http::HeaderValue::from_static("1"))
        );
    }

    #[tokio::test]
    async fn validation_errors_remain_actionable() {
        let response =
            FetchError::BadRequest("`limit` must be between 1 and 100".into()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json error");
        assert_eq!(value["code"], "invalid_request");
        assert_eq!(value["error"], "`limit` must be between 1 and 100");
    }

    #[test]
    fn compact_body_truncates_multibyte_text_at_a_character_boundary() {
        let body = format!("{}é-secret", "a".repeat(511));
        let compacted = compact_body(&body);

        assert_eq!(compacted, format!("{}...", "a".repeat(511)));
        assert!(compacted.is_char_boundary(compacted.len()));
        assert!(!compacted.contains("secret"));
    }
}
