//! Central error definitions and HTTP mapping for resolver failures.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use livy_provenance_sdk::ProvenanceClientError;
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
}

impl ResolverCreditsError {
    pub fn is_payment_required(&self) -> bool {
        match self {
            Self::Backend { status, body } => {
                *status == StatusCode::PAYMENT_REQUIRED
                    || backend_error_code(body).as_deref() == Some("insufficient_user_credits")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("{0} must be set when provenance is enabled")]
    MissingEnv(&'static str),
    #[error("invalid provenance configuration: {0}")]
    InvalidEnv(String),
    #[error("provenance SDK failed: {0}")]
    Sdk(#[from] ProvenanceClientError),
    #[error("provenance HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provenance backend returned {status}: {body}")]
    Backend { status: StatusCode, body: String },
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
            FetchError::UnableFetch(_) | FetchError::Upstream(_) => (
                StatusCode::BAD_GATEWAY,
                "upstream_fetch_failed",
                "Upstream fetch failed".to_string(),
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
    format!("{}...", &body[..512])
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
}
