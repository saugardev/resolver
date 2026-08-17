//! Livy user-credit debit client for OAuth-protected resolver requests.

use crate::auth::ResolverAuthContext;
use crate::errors::ResolverCreditsError;
use livy_provenance_sdk::DEFAULT_LIVY_API_BASE_URL;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static IDEMPOTENCY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ResolverCreditsClient {
    enabled: bool,
    backend_base_url: String,
    amount: i64,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolverCreditDebitOutcome {
    pub mode: String,
    pub enforced: bool,
    pub charged: bool,
    pub amount: i64,
}

impl ResolverCreditsClient {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("LIVY_RESOLVER_CREDITS_ENABLED").unwrap_or(true),
            backend_base_url: backend_base_url_from_env(),
            amount: resolver_request_credit_amount_from_env(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("resolver credits HTTP client should initialize"),
        }
    }

    pub async fn debit_fetch_source(
        &self,
        auth_context: &ResolverAuthContext,
        source_url: &str,
        requested_idempotency_key: Option<&str>,
    ) -> Result<Option<ResolverCreditDebitOutcome>, ResolverCreditsError> {
        self.debit_request(
            auth_context,
            ResolverCreditRequest {
                reason: "resolver.fetch_source",
                route: "mcp.fetch_source",
                source_url: Some(source_url),
                subject_id: None,
                requested_idempotency_key,
                metadata: json!({
                    "tool": "fetch_source",
                }),
            },
        )
        .await
    }

    pub async fn debit_product_request(
        &self,
        auth_context: &ResolverAuthContext,
        route: &str,
        source_url: Option<&str>,
        subject_id: Option<&str>,
    ) -> Result<Option<ResolverCreditDebitOutcome>, ResolverCreditsError> {
        self.debit_request(
            auth_context,
            ResolverCreditRequest {
                reason: "resolver.product_request",
                route,
                source_url,
                subject_id,
                requested_idempotency_key: None,
                metadata: json!({
                    "product_route": route,
                }),
            },
        )
        .await
    }

    async fn debit_request(
        &self,
        auth_context: &ResolverAuthContext,
        request: ResolverCreditRequest<'_>,
    ) -> Result<Option<ResolverCreditDebitOutcome>, ResolverCreditsError> {
        if !self.enabled {
            return Ok(None);
        }

        let Some(access_token) = auth_context.access_token.as_deref() else {
            return Ok(None);
        };
        let tenant_id = auth_context
            .tenant_id
            .as_deref()
            .ok_or(ResolverCreditsError::MissingAuth("tenant_id"))?;
        let project_id = auth_context.project_id.as_deref();
        let idempotency_key = resolver_request_idempotency_key(&request, auth_context);
        let source_sha256 = request
            .source_url
            .map(|source_url| sha256_hex(source_url.as_bytes()));
        let mut metadata = object_or_empty(request.metadata);
        metadata.insert("service".to_string(), json!("livy-resolver"));
        metadata.insert("route".to_string(), json!(request.route));
        metadata.insert(
            "client_id".to_string(),
            json!(auth_context.client_id.as_deref()),
        );
        metadata.insert("scopes".to_string(), json!(&auth_context.scopes));
        metadata.insert("audiences".to_string(), json!(&auth_context.audiences));
        if let Some(source_sha256) = source_sha256 {
            metadata.insert("source_sha256".to_string(), json!(source_sha256));
        }
        if let Some(subject_id) = request.subject_id {
            metadata.insert("subject_id".to_string(), json!(subject_id));
        }

        let response = self
            .http
            .post(self.endpoint(tenant_id))
            .headers(self.headers(access_token)?)
            .json(&json!({
                "amount": self.amount,
                "project_id": project_id,
                "idempotency_key": idempotency_key,
                "reason": request.reason,
                "metadata": metadata
            }))
            .send()
            .await
            .map_err(ResolverCreditsError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ResolverCreditsError::Backend { status, body });
        }

        response
            .json::<ResolverCreditDebitOutcome>()
            .await
            .map(Some)
            .map_err(ResolverCreditsError::Http)
    }

    fn endpoint(&self, tenant_id: &str) -> String {
        format!(
            "{}/api/v1/tenants/{}/users/me/credits/debits",
            self.backend_base_url, tenant_id
        )
    }

    fn headers(&self, access_token: &str) -> Result<HeaderMap, ResolverCreditsError> {
        let mut headers = HeaderMap::new();
        let auth = HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|err| ResolverCreditsError::InvalidHeader(err.to_string()))?;
        headers.insert(AUTHORIZATION, auth);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

struct ResolverCreditRequest<'a> {
    reason: &'a str,
    route: &'a str,
    source_url: Option<&'a str>,
    subject_id: Option<&'a str>,
    requested_idempotency_key: Option<&'a str>,
    metadata: serde_json::Value,
}

fn resolver_request_idempotency_key(
    request: &ResolverCreditRequest<'_>,
    auth_context: &ResolverAuthContext,
) -> String {
    if let Some(requested) = request
        .requested_idempotency_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return format!("resolver_{}:{requested}", key_segment(request.route));
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let sequence = IDEMPOTENCY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let material = json!({
        "tenant_id": auth_context.tenant_id.as_deref(),
        "project_id": auth_context.project_id.as_deref(),
        "client_id": auth_context.client_id.as_deref(),
        "route": request.route,
        "reason": request.reason,
        "source_sha256": request.source_url.map(|source_url| sha256_hex(source_url.as_bytes())),
        "subject_id": request.subject_id,
        "nonce": nonce,
        "sequence": sequence,
    });
    format!(
        "resolver_{}:{}",
        key_segment(request.route),
        sha256_hex(material.to_string().as_bytes())
    )
}

fn object_or_empty(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn key_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            'a'..='z' | '0'..='9' => character,
            'A'..='Z' => character.to_ascii_lowercase(),
            _ => '_',
        })
        .collect()
}

fn backend_base_url_from_env() -> String {
    optional_env("LIVY_BACKEND_BASE_URL")
        .or_else(|| optional_env("RWA_BACKEND_BASE_URL"))
        .or_else(|| optional_env("LIVY_API_BASE_URL"))
        .map(|value| trim_trailing_slash(&value))
        .unwrap_or_else(|| DEFAULT_LIVY_API_BASE_URL.to_string())
}

fn resolver_request_credit_amount_from_env() -> i64 {
    env_i64("LIVY_RESOLVER_REQUEST_CREDIT_COST")
        .or_else(|| env_i64("LIVY_RESOLVER_FETCH_SOURCE_CREDIT_COST"))
        .unwrap_or(1)
}

fn env_bool(name: &str) -> Option<bool> {
    optional_env(name).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    })
}

fn env_i64(name: &str) -> Option<i64> {
    optional_env(name)
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn idempotency_key_uses_client_supplied_value_when_present() {
        let auth = ResolverAuthContext {
            access_token: Some("token".to_string()),
            client_id: Some("client".to_string()),
            scopes: vec!["tool:fetch_source".to_string()],
            audiences: Vec::new(),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
        };

        assert_eq!(
            resolver_request_idempotency_key(
                &ResolverCreditRequest {
                    reason: "resolver.fetch_source",
                    route: "mcp.fetch_source",
                    source_url: Some("https://example.com"),
                    subject_id: None,
                    requested_idempotency_key: Some(" request-1 "),
                    metadata: json!({}),
                },
                &auth,
            ),
            "resolver_mcp_fetch_source:request-1"
        );
    }

    #[test]
    fn idempotency_key_generates_prefixed_fallback() {
        let auth = ResolverAuthContext {
            access_token: Some("token".to_string()),
            client_id: Some("client".to_string()),
            scopes: vec!["tool:fetch_source".to_string()],
            audiences: Vec::new(),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
        };
        let key = resolver_request_idempotency_key(
            &ResolverCreditRequest {
                reason: "resolver.fetch_source",
                route: "mcp.fetch_source",
                source_url: Some("https://example.com"),
                subject_id: None,
                requested_idempotency_key: None,
                metadata: json!({}),
            },
            &auth,
        );

        assert!(key.starts_with("resolver_mcp_fetch_source:"));
        assert!(key.len() > "resolver_mcp_fetch_source:".len());
    }

    #[test]
    fn backend_insufficient_credit_signal_is_payment_required() {
        let status_signal = ResolverCreditsError::Backend {
            status: StatusCode::PAYMENT_REQUIRED,
            body: r#"{"code":"insufficient_user_credits"}"#.to_string(),
        };
        assert!(status_signal.is_payment_required());

        let code_signal = ResolverCreditsError::Backend {
            status: StatusCode::BAD_REQUEST,
            body: r#"{"code":"insufficient_user_credits"}"#.to_string(),
        };
        assert!(code_signal.is_payment_required());

        let unrelated = ResolverCreditsError::Backend {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: r#"{"error":"database unavailable"}"#.to_string(),
        };
        assert!(!unrelated.is_payment_required());
    }
}
