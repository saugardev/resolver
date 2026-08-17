//! Livy user-credit debit client for OAuth-protected resolver requests.

use crate::auth::ResolverAuthContext;
use crate::errors::ResolverCreditsError;
use livy_provenance_sdk::DEFAULT_LIVY_API_BASE_URL;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static IDEMPOTENCY_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const PRICING_VERSION: &str = "resolver-pricing-v1";
const IDEMPOTENCY_BINDING_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const IDEMPOTENCY_BINDING_MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone)]
pub struct ResolverCreditsClient {
    enabled: bool,
    backend_base_url: String,
    amount: i64,
    http: reqwest::Client,
    idempotency_bindings: IdempotencyBindingRegistry,
}

#[derive(Debug, Clone)]
struct IdempotencyBindingRegistry {
    entries: Arc<Mutex<HashMap<String, IdempotencyBinding>>>,
    ttl: Duration,
    max_entries: usize,
}

#[derive(Debug, Clone)]
struct IdempotencyBinding {
    request_fingerprint: String,
    inserted_at: Instant,
    captured: bool,
}

impl Default for IdempotencyBindingRegistry {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            ttl: IDEMPOTENCY_BINDING_TTL,
            max_entries: IDEMPOTENCY_BINDING_MAX_ENTRIES,
        }
    }
}

impl IdempotencyBindingRegistry {
    fn bind(
        &self,
        caller_scope: &str,
        request_fingerprint: &str,
    ) -> Result<bool, ResolverCreditsError> {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ResolverCreditsError::IdempotencyRegistry("lock poisoned".to_string()))?;
        self.prune_locked(&mut entries, now);

        if let Some(existing) = entries.get(caller_scope) {
            return if existing.request_fingerprint == request_fingerprint {
                Ok(existing.captured)
            } else {
                Err(ResolverCreditsError::IdempotencyConflict)
            };
        }

        entries.insert(
            caller_scope.to_string(),
            IdempotencyBinding {
                request_fingerprint: request_fingerprint.to_string(),
                inserted_at: now,
                captured: false,
            },
        );
        self.prune_locked(&mut entries, now);
        Ok(false)
    }

    fn mark_captured(
        &self,
        caller_scope: &str,
        request_fingerprint: &str,
    ) -> Result<(), ResolverCreditsError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ResolverCreditsError::IdempotencyRegistry("lock poisoned".to_string()))?;
        let entry = entries.get_mut(caller_scope).ok_or_else(|| {
            ResolverCreditsError::IdempotencyRegistry("binding missing".to_string())
        })?;
        if entry.request_fingerprint != request_fingerprint {
            return Err(ResolverCreditsError::IdempotencyConflict);
        }
        entry.captured = true;
        Ok(())
    }

    fn prune_locked(&self, entries: &mut HashMap<String, IdempotencyBinding>, now: Instant) {
        entries.retain(|_, entry| now.duration_since(entry.inserted_at) <= self.ttl);
        if self.max_entries == 0 {
            entries.clear();
            return;
        }
        while entries.len() > self.max_entries {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolverCreditDebitOutcome {
    pub mode: String,
    pub enforced: bool,
    pub charged: bool,
    pub amount: i64,
}

#[derive(Debug)]
pub struct ResolverCreditAuthorization {
    access_token: String,
    tenant_id: String,
    project_id: String,
    amount: i64,
    reason: String,
    idempotency_key: String,
    caller_scope: Option<String>,
    request_fingerprint: String,
    metadata: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResolverCreditBalance {
    balance: i64,
}

impl ResolverCreditDebitOutcome {
    pub fn permits_work(&self) -> bool {
        self.enforced || (self.mode == "idempotent_replay" && !self.charged)
    }

    fn is_trusted_for(&self, expected_amount: i64) -> bool {
        self.amount == expected_amount && self.permits_work()
    }
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
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        }
    }

    pub async fn preflight_fetch_source(
        &self,
        auth_context: &ResolverAuthContext,
        source_url: &str,
        logical_request: &Value,
        requested_idempotency_key: Option<&str>,
    ) -> Result<Option<ResolverCreditAuthorization>, ResolverCreditsError> {
        self.preflight_request(
            auth_context,
            ResolverCreditRequest {
                reason: "resolver.fetch_source",
                route: "mcp.fetch_source",
                source_url: Some(source_url),
                subject_id: None,
                logical_request,
                requested_idempotency_key,
                metadata: json!({
                    "tool": "fetch_source",
                }),
            },
        )
        .await
    }

    pub async fn preflight_product_request(
        &self,
        auth_context: &ResolverAuthContext,
        route: &str,
        source_url: Option<&str>,
        subject_id: Option<&str>,
        logical_request: &Value,
        requested_idempotency_key: Option<&str>,
    ) -> Result<Option<ResolverCreditAuthorization>, ResolverCreditsError> {
        self.preflight_request(
            auth_context,
            ResolverCreditRequest {
                reason: "resolver.product_request",
                route,
                source_url,
                subject_id,
                logical_request,
                requested_idempotency_key,
                metadata: json!({
                    "product_route": route,
                }),
            },
        )
        .await
    }

    async fn preflight_request(
        &self,
        auth_context: &ResolverAuthContext,
        request: ResolverCreditRequest<'_>,
    ) -> Result<Option<ResolverCreditAuthorization>, ResolverCreditsError> {
        if !self.enabled {
            return Ok(None);
        }

        let access_token = auth_context
            .access_token
            .as_deref()
            .ok_or(ResolverCreditsError::MissingAuth("access_token"))?;
        auth_context
            .subject
            .as_deref()
            .filter(|subject| !subject.trim().is_empty())
            .ok_or(ResolverCreditsError::MissingAuth("subject"))?;
        let tenant_id = auth_context
            .tenant_id
            .as_deref()
            .ok_or(ResolverCreditsError::MissingAuth("tenant_id"))?;
        let project_id = auth_context
            .project_id
            .as_deref()
            .ok_or(ResolverCreditsError::MissingAuth("project_id"))?;
        let amount = resolver_route_credit_amount_from_env(request.route, self.amount);
        let request_fingerprint = resolver_request_fingerprint(&request, auth_context, amount)?;
        let requested_idempotency_key = request
            .requested_idempotency_key
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut caller_scope = None;
        if let Some(requested) = requested_idempotency_key {
            let scope = caller_idempotency_scope(auth_context, requested)?;
            let already_finalized = self
                .idempotency_bindings
                .bind(&scope, &request_fingerprint)?;
            if already_finalized {
                return Err(ResolverCreditsError::IdempotencyAlreadyFinalized);
            }
            caller_scope = Some(scope);
        }
        let idempotency_key = resolver_request_idempotency_key(
            request.route,
            &request_fingerprint,
            requested_idempotency_key.is_some(),
        );
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
        metadata.insert(
            "request_fingerprint".to_string(),
            json!(&request_fingerprint),
        );
        metadata.insert("pricing_version".to_string(), json!(PRICING_VERSION));
        if let Some(requested) = requested_idempotency_key {
            metadata.insert(
                "caller_idempotency_key_sha256".to_string(),
                json!(sha256_hex(requested.as_bytes())),
            );
        }
        if let Some(source_sha256) = source_sha256 {
            metadata.insert("source_sha256".to_string(), json!(source_sha256));
        }
        if let Some(subject_id) = request.subject_id {
            metadata.insert("subject_id".to_string(), json!(subject_id));
        }

        self.require_available_balance(tenant_id, access_token, amount)
            .await?;

        Ok(Some(ResolverCreditAuthorization {
            access_token: access_token.to_string(),
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            amount,
            reason: request.reason.to_string(),
            idempotency_key,
            caller_scope,
            request_fingerprint,
            metadata,
        }))
    }

    pub async fn capture_authorized_request(
        &self,
        authorization: Option<ResolverCreditAuthorization>,
    ) -> Result<Option<ResolverCreditDebitOutcome>, ResolverCreditsError> {
        let Some(authorization) = authorization else {
            return Ok(None);
        };
        let response = self
            .http
            .post(self.debit_endpoint(&authorization.tenant_id))
            .headers(self.headers(&authorization.access_token)?)
            .json(&json!({
                "amount": authorization.amount,
                "project_id": authorization.project_id,
                "idempotency_key": authorization.idempotency_key,
                "reason": authorization.reason,
                "metadata": authorization.metadata,
            }))
            .send()
            .await
            .map_err(ResolverCreditsError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ResolverCreditsError::Backend { status, body });
        }
        let outcome = response
            .json::<ResolverCreditDebitOutcome>()
            .await
            .map_err(ResolverCreditsError::Http)?;
        if !outcome.is_trusted_for(authorization.amount) {
            return Err(ResolverCreditsError::UntrustedDebitOutcome {
                mode: outcome.mode,
                enforced: outcome.enforced,
                charged: outcome.charged,
                amount: outcome.amount,
                expected_amount: authorization.amount,
            });
        }
        if let Some(caller_scope) = authorization.caller_scope.as_deref() {
            self.idempotency_bindings
                .mark_captured(caller_scope, &authorization.request_fingerprint)?;
        }
        Ok(Some(outcome))
    }

    async fn require_available_balance(
        &self,
        tenant_id: &str,
        access_token: &str,
        amount: i64,
    ) -> Result<(), ResolverCreditsError> {
        let response = self
            .http
            .get(self.balance_endpoint(tenant_id))
            .headers(self.headers(access_token)?)
            .send()
            .await
            .map_err(ResolverCreditsError::Http)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ResolverCreditsError::Backend { status, body });
        }
        let balance = response
            .json::<ResolverCreditBalance>()
            .await
            .map_err(ResolverCreditsError::Http)?;
        if balance.balance >= amount {
            return Ok(());
        }
        Err(ResolverCreditsError::InsufficientCredits {
            balance: balance.balance,
            required: amount,
        })
    }

    fn debit_endpoint(&self, tenant_id: &str) -> String {
        format!(
            "{}/api/v1/tenants/{}/users/me/credits/debits",
            self.backend_base_url, tenant_id
        )
    }

    fn balance_endpoint(&self, tenant_id: &str) -> String {
        format!(
            "{}/api/v1/tenants/{}/users/me/credits",
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
    logical_request: &'a Value,
    requested_idempotency_key: Option<&'a str>,
    metadata: serde_json::Value,
}

fn resolver_request_fingerprint(
    request: &ResolverCreditRequest<'_>,
    auth_context: &ResolverAuthContext,
    amount: i64,
) -> Result<String, ResolverCreditsError> {
    let material = json!({
        "contract": "livy-resolver-debit-fingerprint-v1",
        "tenant_id": auth_context.tenant_id.as_deref(),
        "project_id": auth_context.project_id.as_deref(),
        "client_id": auth_context.client_id.as_deref(),
        "subject": auth_context.subject.as_deref(),
        "reason": request.reason,
        "route": request.route,
        "amount": amount,
        "pricing_version": PRICING_VERSION,
        "caller_idempotency_key": request
            .requested_idempotency_key
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        "logical_request": request.logical_request,
    });
    let canonical = canonical_json_bytes(&material)?;
    Ok(sha256_hex(&canonical))
}

fn caller_idempotency_scope(
    auth_context: &ResolverAuthContext,
    requested_idempotency_key: &str,
) -> Result<String, ResolverCreditsError> {
    let material = json!({
        "contract": "livy-resolver-caller-idempotency-scope-v1",
        "tenant_id": auth_context.tenant_id.as_deref(),
        "project_id": auth_context.project_id.as_deref(),
        "client_id": auth_context.client_id.as_deref(),
        "subject": auth_context.subject.as_deref(),
        "caller_idempotency_key": requested_idempotency_key,
    });
    let canonical = canonical_json_bytes(&material)?;
    Ok(sha256_hex(&canonical))
}

fn resolver_request_idempotency_key(
    route: &str,
    request_fingerprint: &str,
    caller_key_present: bool,
) -> String {
    if caller_key_present {
        return format!("resolver_{}:{request_fingerprint}", key_segment(route),);
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let sequence = IDEMPOTENCY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let material = json!({
        "request_fingerprint": request_fingerprint,
        "nonce": nonce,
        "sequence": sequence,
    });
    format!(
        "resolver_{}:{}",
        key_segment(route),
        sha256_hex(material.to_string().as_bytes())
    )
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, ResolverCreditsError> {
    serde_json::to_vec(&canonical_json(value)).map_err(|err| {
        ResolverCreditsError::IdempotencyRegistry(format!(
            "logical request canonicalization failed: {err}"
        ))
    })
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&object[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
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

fn resolver_route_credit_amount_from_env(route: &str, default: i64) -> i64 {
    env_i64(&route_credit_cost_env_name(route)).unwrap_or(default)
}

fn route_credit_cost_env_name(route: &str) -> String {
    let suffix = route
        .chars()
        .map(|character| match character {
            'a'..='z' => character.to_ascii_uppercase(),
            'A'..='Z' | '0'..='9' => character,
            _ => '_',
        })
        .collect::<String>();
    format!("LIVY_RESOLVER_CREDIT_COST_{suffix}")
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
    use axum::{
        Json, Router, extract::Path, http::HeaderMap as AxumHeaderMap, routing::get, routing::post,
    };
    use reqwest::StatusCode;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    };

    fn auth() -> ResolverAuthContext {
        ResolverAuthContext {
            access_token: Some("token".to_string()),
            subject: Some("user-a".to_string()),
            client_id: Some("client".to_string()),
            scopes: vec!["tool:fetch_source".to_string()],
            audiences: Vec::new(),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
        }
    }

    fn credit_request<'a>(
        logical_request: &'a Value,
        caller_key: &'a str,
    ) -> ResolverCreditRequest<'a> {
        ResolverCreditRequest {
            reason: "resolver.fetch_source",
            route: "mcp.fetch_source",
            source_url: Some("https://example.com"),
            subject_id: None,
            logical_request,
            requested_idempotency_key: Some(caller_key),
            metadata: json!({}),
        }
    }

    #[test]
    fn fingerprint_is_canonical_and_binds_every_billing_boundary() {
        let logical = json!({
            "source": "https://example.com/a",
            "options": {"limit": 3, "mode": "fast"},
        });
        let reordered = json!({
            "options": {"mode": "fast", "limit": 3},
            "source": "https://example.com/a",
        });
        let request = credit_request(&logical, "retry-1");
        let same = credit_request(&reordered, "retry-1");
        let first = resolver_request_fingerprint(&request, &auth(), 7).unwrap();
        assert_eq!(
            first,
            resolver_request_fingerprint(&same, &auth(), 7).unwrap()
        );

        let changed_source = json!({
            "source": "https://example.com/b",
            "options": {"limit": 3, "mode": "fast"},
        });
        assert_ne!(
            first,
            resolver_request_fingerprint(&credit_request(&changed_source, "retry-1"), &auth(), 7)
                .unwrap()
        );

        let changed_options = json!({
            "source": "https://example.com/a",
            "options": {"limit": 4, "mode": "fast"},
        });
        assert_ne!(
            first,
            resolver_request_fingerprint(&credit_request(&changed_options, "retry-1"), &auth(), 7)
                .unwrap()
        );

        let mut changed_route = credit_request(&logical, "retry-1");
        changed_route.route = "fetch";
        assert_ne!(
            first,
            resolver_request_fingerprint(&changed_route, &auth(), 7).unwrap()
        );
        assert_ne!(
            first,
            resolver_request_fingerprint(&request, &auth(), 8).unwrap()
        );
        assert_ne!(
            first,
            resolver_request_fingerprint(&credit_request(&logical, "retry-2"), &auth(), 7).unwrap()
        );

        let mut changed_auth = auth();
        changed_auth.project_id = Some("project-b".to_string());
        assert_ne!(
            first,
            resolver_request_fingerprint(&request, &changed_auth, 7).unwrap()
        );

        let mut changed_subject = auth();
        changed_subject.subject = Some("user-b".to_string());
        assert_ne!(
            first,
            resolver_request_fingerprint(&request, &changed_subject, 7).unwrap()
        );
    }

    #[test]
    fn caller_key_binding_tracks_pending_retry_and_rejects_different_request() {
        let registry = IdempotencyBindingRegistry {
            entries: Arc::new(Mutex::new(HashMap::new())),
            ttl: Duration::from_secs(60),
            max_entries: 4,
        };
        assert!(!registry.bind("caller-scope", "fingerprint-a").unwrap());
        assert!(!registry.bind("caller-scope", "fingerprint-a").unwrap());
        assert!(matches!(
            registry.bind("caller-scope", "fingerprint-b"),
            Err(ResolverCreditsError::IdempotencyConflict)
        ));
    }

    #[test]
    fn caller_key_backend_key_is_the_complete_request_fingerprint() {
        let fingerprint = "abcd";
        assert_eq!(
            resolver_request_idempotency_key("mcp.fetch_source", fingerprint, true),
            "resolver_mcp_fetch_source:abcd"
        );
    }

    #[test]
    fn only_enforced_or_idempotent_replay_outcomes_permit_work() {
        let outcome = |mode: &str, enforced| ResolverCreditDebitOutcome {
            mode: mode.to_string(),
            enforced,
            charged: false,
            amount: 1,
        };

        assert!(outcome("charged", true).permits_work());
        assert!(outcome("idempotent_replay", false).permits_work());
        assert!(!outcome("advisory", false).permits_work());
    }

    #[test]
    fn route_credit_costs_have_independent_stable_keys() {
        assert_eq!(
            route_credit_cost_env_name("fetch"),
            "LIVY_RESOLVER_CREDIT_COST_FETCH"
        );
        assert_eq!(
            route_credit_cost_env_name("mcp.fetch_source"),
            "LIVY_RESOLVER_CREDIT_COST_MCP_FETCH_SOURCE"
        );
    }

    #[tokio::test]
    async fn product_debit_sends_scoped_idempotency_and_project_context() {
        let captured = Arc::new(Mutex::new(None));
        let captured_for_handler = captured.clone();
        let app = Router::new()
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits",
                get(|| async { Json(json!({"balance": 10})) }),
            )
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits/debits",
                post(
                    move |Path(tenant_id): Path<String>,
                          headers: AxumHeaderMap,
                          Json(body): Json<serde_json::Value>| {
                        let captured = captured_for_handler.clone();
                        async move {
                            *captured.lock().expect("capture lock") = Some(json!({
                                "tenant_id": tenant_id,
                                "authorization": headers
                                    .get(AUTHORIZATION)
                                    .and_then(|value| value.to_str().ok()),
                                "body": body,
                            }));
                            Json(json!({
                                "mode": "charged",
                                "enforced": true,
                                "charged": true,
                                "amount": 7,
                            }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = ResolverCreditsClient {
            enabled: true,
            backend_base_url: format!("http://{address}"),
            amount: 7,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        };
        let auth = ResolverAuthContext {
            access_token: Some("oauth-token".to_string()),
            subject: Some("user-a".to_string()),
            client_id: Some("client-a".to_string()),
            scopes: vec!["resolver:source:fetch".to_string()],
            audiences: vec!["resolver".to_string()],
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
        };
        let logical_request = json!({
            "contract": "livy-resolver-logical-request-v1",
            "route": "integration_test",
            "source": "https://example.com",
            "upstream_parameters": {"limit": 2},
        });
        let authorization = client
            .preflight_product_request(
                &auth,
                "integration_test",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("retry-1"),
            )
            .await
            .unwrap();
        assert!(
            captured.lock().expect("capture lock").as_ref().is_none(),
            "preflight must not debit"
        );
        let outcome = client
            .capture_authorized_request(authorization)
            .await
            .unwrap()
            .unwrap();
        assert!(outcome.permits_work());

        let expected_request = ResolverCreditRequest {
            reason: "resolver.product_request",
            route: "integration_test",
            source_url: Some("https://example.com"),
            subject_id: None,
            logical_request: &logical_request,
            requested_idempotency_key: Some("retry-1"),
            metadata: json!({}),
        };
        let expected_fingerprint =
            resolver_request_fingerprint(&expected_request, &auth, 7).unwrap();
        let expected_key =
            resolver_request_idempotency_key("integration_test", &expected_fingerprint, true);
        let request = captured
            .lock()
            .expect("capture lock")
            .clone()
            .expect("request was captured");
        assert_eq!(request["tenant_id"], "tenant-a");
        assert_eq!(request["authorization"], "Bearer oauth-token");
        assert_eq!(request["body"]["project_id"], "project-a");
        assert_eq!(request["body"]["amount"], 7);
        assert_eq!(request["body"]["idempotency_key"], expected_key);
        assert_eq!(
            request["body"]["metadata"]["request_fingerprint"],
            expected_fingerprint
        );
        assert_eq!(
            request["body"]["metadata"]["pricing_version"],
            PRICING_VERSION
        );
        assert_eq!(
            request["body"]["metadata"]["caller_idempotency_key_sha256"],
            sha256_hex(b"retry-1")
        );

        server.abort();
    }

    #[tokio::test]
    async fn locally_finalized_request_stops_before_balance_or_work() {
        let balance_checks = Arc::new(AtomicUsize::new(0));
        let debit_calls = Arc::new(AtomicUsize::new(0));
        let checks_for_handler = balance_checks.clone();
        let debits_for_handler = debit_calls.clone();
        let app = Router::new()
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits",
                get(move || {
                    let checks = checks_for_handler.clone();
                    async move {
                        checks.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"balance": 10}))
                    }
                }),
            )
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits/debits",
                post(move || {
                    let debits = debits_for_handler.clone();
                    async move {
                        debits.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "mode": "enforce",
                            "enforced": true,
                            "charged": true,
                            "amount": 1,
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ResolverCreditsClient {
            enabled: true,
            backend_base_url: format!("http://{address}"),
            amount: 1,
            http: reqwest::Client::new(),
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        };
        let auth = auth();
        let logical_request = json!({"source": "https://example.com"});

        let authorization = client
            .preflight_product_request(
                &auth,
                "finalized_regression",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("same-key"),
            )
            .await
            .unwrap();
        client
            .capture_authorized_request(authorization)
            .await
            .unwrap();

        assert!(matches!(
            client
                .preflight_product_request(
                    &auth,
                    "finalized_regression",
                    Some("https://example.com"),
                    None,
                    &logical_request,
                    Some("same-key"),
                )
                .await,
            Err(ResolverCreditsError::IdempotencyAlreadyFinalized)
        ));
        assert_eq!(balance_checks.load(Ordering::SeqCst), 1);
        assert_eq!(debit_calls.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn unenforced_success_does_not_mark_binding_captured_for_retry() {
        let balance_checks = Arc::new(AtomicUsize::new(0));
        let checks_for_handler = balance_checks.clone();
        let app = Router::new()
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits",
                get(move || {
                    let checks = checks_for_handler.clone();
                    async move {
                        checks.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"balance": 10}))
                    }
                }),
            )
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits/debits",
                post(|| async {
                    Json(json!({
                        "mode": "shadow",
                        "enforced": false,
                        "charged": true,
                        "amount": 1,
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ResolverCreditsClient {
            enabled: true,
            backend_base_url: format!("http://{address}"),
            amount: 1,
            http: reqwest::Client::new(),
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        };
        let auth = auth();
        let logical_request = json!({"source": "https://example.com"});

        let authorization = client
            .preflight_product_request(
                &auth,
                "retry_regression",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("same-key"),
            )
            .await
            .unwrap();
        assert!(matches!(
            client.capture_authorized_request(authorization).await,
            Err(ResolverCreditsError::UntrustedDebitOutcome { .. })
        ));

        client
            .preflight_product_request(
                &auth,
                "retry_regression",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("same-key"),
            )
            .await
            .unwrap();
        assert_eq!(balance_checks.load(Ordering::SeqCst), 2);

        server.abort();
    }

    #[tokio::test]
    async fn captured_binding_is_never_shared_between_oauth_subjects() {
        let user_b_balance_checks = Arc::new(AtomicUsize::new(0));
        let checks_for_handler = user_b_balance_checks.clone();
        let app = Router::new()
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits",
                get(move |headers: AxumHeaderMap| {
                    let checks = checks_for_handler.clone();
                    async move {
                        let user_b = headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer token-b");
                        if user_b {
                            checks.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"balance": 0}))
                        } else {
                            Json(json!({"balance": 10}))
                        }
                    }
                }),
            )
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits/debits",
                post(|| async {
                    Json(json!({
                        "mode": "enforce",
                        "enforced": true,
                        "charged": true,
                        "amount": 1,
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ResolverCreditsClient {
            enabled: true,
            backend_base_url: format!("http://{address}"),
            amount: 1,
            http: reqwest::Client::new(),
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        };
        let auth_a = auth();
        let mut auth_b = auth_a.clone();
        auth_b.access_token = Some("token-b".to_string());
        auth_b.subject = Some("user-b".to_string());
        let logical_request = json!({"source": "https://example.com"});

        let user_a_authorization = client
            .preflight_product_request(
                &auth_a,
                "subject_regression",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("same-key"),
            )
            .await
            .unwrap();
        client
            .capture_authorized_request(user_a_authorization)
            .await
            .unwrap();

        assert!(matches!(
            client
                .preflight_product_request(
                    &auth_b,
                    "subject_regression",
                    Some("https://example.com"),
                    None,
                    &logical_request,
                    Some("same-key"),
                )
                .await,
            Err(ResolverCreditsError::InsufficientCredits { .. })
        ));
        assert_eq!(user_b_balance_checks.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn shadow_ledger_row_never_authorizes_retry_after_restart() {
        #[derive(Default)]
        struct BackendState {
            balance: AtomicI64,
            balance_checks: AtomicUsize,
            debit_calls: AtomicUsize,
            ledger_calls: AtomicUsize,
            last_debit: Mutex<Option<Value>>,
        }

        let backend = Arc::new(BackendState {
            balance: AtomicI64::new(1),
            ..BackendState::default()
        });
        let balance_backend = backend.clone();
        let debit_backend = backend.clone();
        let ledger_backend = backend.clone();
        let app = Router::new()
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits",
                get(move || {
                    let backend = balance_backend.clone();
                    async move {
                        backend.balance_checks.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"balance": backend.balance.load(Ordering::SeqCst)}))
                    }
                }),
            )
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credit-ledger",
                get(move || {
                    let backend = ledger_backend.clone();
                    async move {
                        backend.ledger_calls.fetch_add(1, Ordering::SeqCst);
                        let debit = backend.last_debit.lock().unwrap().clone();
                        let row = debit.map(|body| {
                            json!({
                                "entry_type": "debit",
                                "idempotency_key": body["idempotency_key"],
                                "metadata": {
                                    "request_fingerprint": body["metadata"]["request_fingerprint"]
                                }
                            })
                        });
                        Json(row.map_or_else(|| json!([]), |row| json!([row])))
                    }
                }),
            )
            .route(
                "/api/v1/tenants/{tenant_id}/users/me/credits/debits",
                post(move |Json(body): Json<Value>| {
                    let backend = debit_backend.clone();
                    async move {
                        backend.debit_calls.fetch_add(1, Ordering::SeqCst);
                        backend.balance.store(0, Ordering::SeqCst);
                        *backend.last_debit.lock().unwrap() = Some(body);
                        Json(json!({
                            "mode": "shadow",
                            "enforced": false,
                            "charged": true,
                            "amount": 1,
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let new_client = || ResolverCreditsClient {
            enabled: true,
            backend_base_url: format!("http://{address}"),
            amount: 1,
            http: reqwest::Client::new(),
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        };
        let auth = auth();
        let logical_request = json!({"source": "https://example.com"});
        let spider_calls = AtomicUsize::new(0);
        let evidence_calls = AtomicUsize::new(0);

        let first_client = new_client();
        let authorization = first_client
            .preflight_product_request(
                &auth,
                "restart_regression",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("same-key"),
            )
            .await
            .unwrap();
        spider_calls.fetch_add(1, Ordering::SeqCst);
        let first_capture = first_client.capture_authorized_request(authorization).await;
        if first_capture.is_ok() {
            evidence_calls.fetch_add(1, Ordering::SeqCst);
        }
        assert!(matches!(
            first_capture,
            Err(ResolverCreditsError::UntrustedDebitOutcome { .. })
        ));
        assert!(backend.last_debit.lock().unwrap().is_some());

        // A new client has no in-memory binding, exactly like a restarted replica.
        let restarted_client = new_client();
        let retry = restarted_client
            .preflight_product_request(
                &auth,
                "restart_regression",
                Some("https://example.com"),
                None,
                &logical_request,
                Some("same-key"),
            )
            .await;
        let retry_error = match retry {
            Ok(authorization) => {
                spider_calls.fetch_add(1, Ordering::SeqCst);
                if restarted_client
                    .capture_authorized_request(authorization)
                    .await
                    .is_ok()
                {
                    evidence_calls.fetch_add(1, Ordering::SeqCst);
                }
                None
            }
            Err(err) => Some(err),
        };

        assert!(matches!(
            retry_error,
            Some(ResolverCreditsError::InsufficientCredits { .. })
        ));
        assert_eq!(spider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.debit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(evidence_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.ledger_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.balance_checks.load(Ordering::SeqCst), 2);

        server.abort();
    }

    #[test]
    fn idempotency_key_generates_prefixed_fallback() {
        let key = resolver_request_idempotency_key("mcp.fetch_source", "fingerprint", false);
        let second = resolver_request_idempotency_key("mcp.fetch_source", "fingerprint", false);

        assert!(key.starts_with("resolver_mcp_fetch_source:"));
        assert!(key.len() > "resolver_mcp_fetch_source:".len());
        assert_ne!(key, second);
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

        let conflict = ResolverCreditsError::Backend {
            status: StatusCode::CONFLICT,
            body: r#"{"code":"idempotency_conflict"}"#.to_string(),
        };
        assert!(conflict.is_idempotency_conflict());
        assert!(ResolverCreditsError::IdempotencyAlreadyFinalized.is_idempotency_conflict());
        assert!(
            ResolverCreditsError::IdempotencyAlreadyFinalized.is_idempotency_already_finalized()
        );
    }
}
