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
use uuid::Uuid;

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
            .map_err(|_| ResolverCreditsError::IdempotencyRegistry("lock poisoned".into()))?;
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
            .map_err(|_| ResolverCreditsError::IdempotencyRegistry("lock poisoned".into()))?;
        let entry = entries
            .get_mut(caller_scope)
            .ok_or_else(|| ResolverCreditsError::IdempotencyRegistry("binding missing".into()))?;
        if entry.request_fingerprint != request_fingerprint {
            return Err(ResolverCreditsError::IdempotencyConflict);
        }
        entry.captured = true;
        Ok(())
    }

    fn prune_locked(&self, entries: &mut HashMap<String, IdempotencyBinding>, now: Instant) {
        entries.retain(|_, entry| now.duration_since(entry.inserted_at) <= self.ttl);
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
    #[serde(default)]
    pub reason: Option<String>,
    pub ledger_entry: Option<ResolverCreditLedgerEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolverCreditLedgerEntry {
    tenant_id: String,
    project_id: Option<String>,
    entry_type: String,
    amount_delta: i64,
    idempotency_key: Option<String>,
    #[serde(default)]
    metadata: Value,
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
    capture_attempt_id: String,
    metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResolverCreditBalance {
    balance: i64,
}

impl ResolverCreditsClient {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("LIVY_RESOLVER_CREDITS_ENABLED").unwrap_or(true),
            backend_base_url: backend_base_url_from_env(),
            amount: resolver_request_credit_amount_from_env(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
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
                metadata: json!({"tool": "fetch_source"}),
            },
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn for_tests(backend_base_url: String) -> Self {
        Self {
            enabled: true,
            backend_base_url: trim_trailing_slash(&backend_base_url),
            amount: 1,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("test credits client"),
            idempotency_bindings: IdempotencyBindingRegistry::default(),
        }
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
                metadata: json!({"product_route": route}),
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
            if self
                .idempotency_bindings
                .bind(&scope, &request_fingerprint)?
            {
                return Err(ResolverCreditsError::IdempotencyAlreadyFinalized);
            }
            caller_scope = Some(scope);
        }

        let idempotency_key = resolver_request_idempotency_key(
            request.route,
            &request_fingerprint,
            requested_idempotency_key.is_some(),
        );

        self.require_available_balance(tenant_id, access_token, amount)
            .await?;

        let capture_attempt_id = Uuid::new_v4().to_string();
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
        metadata.insert("capture_attempt_id".to_string(), json!(&capture_attempt_id));
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

        Ok(Some(ResolverCreditAuthorization {
            access_token: access_token.to_string(),
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            amount,
            reason: request.reason.to_string(),
            idempotency_key,
            caller_scope,
            request_fingerprint,
            capture_attempt_id,
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
        if !outcome.charged || !outcome.enforced || outcome.amount != authorization.amount {
            return Err(ResolverCreditsError::CaptureNotApplied(
                outcome.reason.clone().unwrap_or_else(|| {
                    "backend returned an uncharged, unenforced, or mismatched outcome".into()
                }),
            ));
        }

        let ledger = outcome.ledger_entry.as_ref().ok_or_else(|| {
            ResolverCreditsError::CaptureNotApplied(
                "backend did not return a request-bound ledger entry".into(),
            )
        })?;
        validate_capture_binding(ledger, &authorization)?;
        let replayed = ledger
            .metadata
            .get("capture_attempt_id")
            .and_then(Value::as_str)
            != Some(authorization.capture_attempt_id.as_str());
        if let Some(scope) = authorization.caller_scope.as_deref() {
            self.idempotency_bindings
                .mark_captured(scope, &authorization.request_fingerprint)?;
        }
        if replayed {
            return Err(ResolverCreditsError::UnsafeReplay);
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
        if balance.balance < amount {
            return Err(ResolverCreditsError::InsufficientCredits {
                available: balance.balance,
                required: amount,
            });
        }
        Ok(())
    }

    fn balance_endpoint(&self, tenant_id: &str) -> String {
        format!(
            "{}/api/v1/tenants/{}/users/me/credits",
            self.backend_base_url, tenant_id
        )
    }

    fn debit_endpoint(&self, tenant_id: &str) -> String {
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
    logical_request: &'a Value,
    requested_idempotency_key: Option<&'a str>,
    metadata: Value,
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
    Ok(sha256_hex(&canonical_json_bytes(&material)?))
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
    Ok(sha256_hex(&canonical_json_bytes(&material)?))
}

fn resolver_request_idempotency_key(
    route: &str,
    request_fingerprint: &str,
    caller_key_present: bool,
) -> String {
    if caller_key_present {
        return format!("resolver_{}:{request_fingerprint}", key_segment(route));
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
        sha256_hex(canonical_json(&material).to_string().as_bytes())
    )
}

fn validate_capture_binding(
    ledger: &ResolverCreditLedgerEntry,
    authorization: &ResolverCreditAuthorization,
) -> Result<(), ResolverCreditsError> {
    let valid = ledger.tenant_id == authorization.tenant_id
        && ledger.project_id.as_deref() == Some(authorization.project_id.as_str())
        && ledger.entry_type == "debit"
        && ledger.amount_delta == -authorization.amount
        && ledger.idempotency_key.as_deref() == Some(authorization.idempotency_key.as_str())
        && ledger
            .metadata
            .get("request_fingerprint")
            .and_then(Value::as_str)
            == Some(authorization.request_fingerprint.as_str())
        && ledger
            .metadata
            .get("pricing_version")
            .and_then(Value::as_str)
            == Some(PRICING_VERSION);
    if valid {
        Ok(())
    } else {
        Err(ResolverCreditsError::CaptureNotApplied(
            "backend debit outcome was not bound to the authorized request".into(),
        ))
    }
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
    use reqwest::StatusCode;

    fn auth(project_id: &str) -> ResolverAuthContext {
        ResolverAuthContext {
            access_token: Some("token".to_string()),
            subject: Some("user-a".to_string()),
            client_id: Some("client".to_string()),
            scopes: vec!["tool:fetch_source".to_string()],
            audiences: Vec::new(),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some(project_id.to_string()),
        }
    }

    fn request<'a>(logical_request: &'a Value, key: Option<&'a str>) -> ResolverCreditRequest<'a> {
        ResolverCreditRequest {
            reason: "resolver.fetch_source",
            route: "mcp.fetch_source",
            source_url: logical_request.get("source").and_then(Value::as_str),
            subject_id: None,
            logical_request,
            requested_idempotency_key: key,
            metadata: json!({}),
        }
    }

    #[test]
    fn fingerprint_and_backend_key_bind_source_project_and_caller_key() {
        let first_logical = json!({"source": "https://example.com/a", "mode": "fast"});
        let reordered = json!({"mode": "fast", "source": "https://example.com/a"});
        let changed_source = json!({"source": "https://example.com/b", "mode": "fast"});
        let first = resolver_request_fingerprint(
            &request(&first_logical, Some("retry-1")),
            &auth("project-a"),
            1,
        )
        .unwrap();
        assert_eq!(
            first,
            resolver_request_fingerprint(
                &request(&reordered, Some("retry-1")),
                &auth("project-a"),
                1,
            )
            .unwrap()
        );
        assert_ne!(
            first,
            resolver_request_fingerprint(
                &request(&changed_source, Some("retry-1")),
                &auth("project-a"),
                1,
            )
            .unwrap()
        );
        assert_ne!(
            first,
            resolver_request_fingerprint(
                &request(&first_logical, Some("retry-1")),
                &auth("project-b"),
                1,
            )
            .unwrap()
        );
        assert_ne!(
            first,
            resolver_request_fingerprint(
                &request(&first_logical, Some("retry-2")),
                &auth("project-a"),
                1,
            )
            .unwrap()
        );
        assert_eq!(
            resolver_request_idempotency_key("mcp.fetch_source", &first, true),
            format!("resolver_mcp_fetch_source:{first}")
        );
    }

    #[test]
    fn caller_key_registry_replays_exact_requests_and_rejects_conflicts() {
        let registry = IdempotencyBindingRegistry {
            entries: Arc::new(Mutex::new(HashMap::new())),
            ttl: Duration::from_secs(60),
            max_entries: 4,
        };
        assert!(!registry.bind("scope", "fingerprint-a").unwrap());
        registry.mark_captured("scope", "fingerprint-a").unwrap();
        assert!(registry.bind("scope", "fingerprint-a").unwrap());
        assert!(matches!(
            registry.bind("scope", "fingerprint-b"),
            Err(ResolverCreditsError::IdempotencyConflict)
        ));
    }

    #[test]
    fn capture_ledger_must_bind_project_amount_key_and_fingerprint() {
        let authorization = ResolverCreditAuthorization {
            access_token: "token".to_string(),
            tenant_id: "tenant-a".to_string(),
            project_id: "project-a".to_string(),
            amount: 2,
            reason: "resolver.product_request".to_string(),
            idempotency_key: "resolver_fetch:fingerprint".to_string(),
            caller_scope: None,
            request_fingerprint: "fingerprint".to_string(),
            capture_attempt_id: "attempt".to_string(),
            metadata: Map::new(),
        };
        let ledger = |project_id: &str, amount_delta: i64, key: &str, fingerprint: &str| {
            ResolverCreditLedgerEntry {
                tenant_id: "tenant-a".to_string(),
                project_id: Some(project_id.to_string()),
                entry_type: "debit".to_string(),
                amount_delta,
                idempotency_key: Some(key.to_string()),
                metadata: json!({
                    "request_fingerprint": fingerprint,
                    "pricing_version": PRICING_VERSION,
                }),
            }
        };

        assert!(
            validate_capture_binding(
                &ledger("project-a", -2, "resolver_fetch:fingerprint", "fingerprint"),
                &authorization,
            )
            .is_ok()
        );
        for mismatched in [
            ledger("project-b", -2, "resolver_fetch:fingerprint", "fingerprint"),
            ledger("project-a", -1, "resolver_fetch:fingerprint", "fingerprint"),
            ledger("project-a", -2, "resolver_fetch:other", "fingerprint"),
            ledger("project-a", -2, "resolver_fetch:fingerprint", "other"),
        ] {
            assert!(validate_capture_binding(&mismatched, &authorization).is_err());
        }
    }

    #[test]
    fn idempotency_key_generates_prefixed_fallback() {
        let key = resolver_request_idempotency_key("mcp.fetch_source", "fingerprint", false);
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
