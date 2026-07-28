//! MCP tool wrapper for exact source fetching.

use crate::auth::{ResolverAuth, ResolverAuthContext};
use crate::credits::ResolverCreditsClient;
use crate::errors::ResolverAuthError;
use crate::fetch::Fetcher;
use crate::types::{FetchWithReceipt, ProvenanceOptions};
use crate::types::{validate_idempotency_key, validate_source_url};
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::tool::Extension,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, Meta},
    schemars, tool, tool_handler, tool_router,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const FETCH_SOURCE_SCOPES: &[&str] = &["tool:fetch_source", "resolver:source:fetch"];
const FETCH_SOURCE_TITLE: &str = "Fetch Source";
const FETCH_SOURCE_INVOCATION_START: &str = "Fetching source";
const FETCH_SOURCE_INVOCATION_DONE: &str = "Source fetched";
const EXPLORER_QUERY_BASE_URL: &str = "https://explorer.livylabs.xyz/?q=";
const MCP_COMPAT_BODY_LIMIT: usize = 1024 * 1024;
const FALLBACK_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const FALLBACK_CACHE_MAX_ENTRIES: usize = 1024;

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct Params {
    #[schemars(
        description = "The exact source URL from the user prompt. Do not replace it with a search query or another URL."
    )]
    pub url: String,
    #[schemars(
        description = "Optional stable idempotency key for this fetch_source call. Reuse the same key only when retrying the same MCP call."
    )]
    pub idempotency_key: Option<String>,
    #[schemars(description = "Create a Livy provenance attestation for this fetch.")]
    pub provenance: Option<bool>,
    #[schemars(description = "Publish the resolver artifact to Arweave. Implies provenance.")]
    pub publish_to_arweave: Option<bool>,
    #[schemars(
        description = "Register the attestation on the configured on-chain registry. Implies provenance."
    )]
    pub register_onchain: Option<bool>,
    #[schemars(description = "Wait for requested publication targets before returning.")]
    pub wait_for_publication: Option<bool>,
}

pub struct Server {
    fetcher: Arc<Fetcher>,
    auth: Arc<ResolverAuth>,
    credits: Arc<ResolverCreditsClient>,
    fallback_cache: Arc<FetchFallbackCache>,
}

#[derive(Debug)]
pub struct FetchFallbackCache {
    entries: Mutex<HashMap<String, FallbackCacheEntry>>,
    ttl: Duration,
    max_entries: usize,
}

#[derive(Debug, Clone)]
struct FallbackCacheEntry {
    inserted_at: Instant,
    reason: &'static str,
}

impl Default for FetchFallbackCache {
    fn default() -> Self {
        Self::new(FALLBACK_CACHE_TTL, FALLBACK_CACHE_MAX_ENTRIES)
    }
}

impl FetchFallbackCache {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            max_entries,
        }
    }

    fn lookup(&self, key: &str) -> Option<&'static str> {
        let now = Instant::now();
        let mut entries = self.entries.lock().ok()?;
        self.prune_locked(&mut entries, now);
        entries.get(key).map(|entry| entry.reason)
    }

    fn flag(&self, key: String, reason: &'static str) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        let now = Instant::now();
        self.prune_locked(&mut entries, now);
        entries.insert(
            key,
            FallbackCacheEntry {
                inserted_at: now,
                reason,
            },
        );
        self.prune_locked(&mut entries, now);
    }

    fn prune_locked(&self, entries: &mut HashMap<String, FallbackCacheEntry>, now: Instant) {
        entries.retain(|_, entry| now.duration_since(entry.inserted_at) <= self.ttl);
        if self.max_entries == 0 {
            entries.clear();
            return;
        }
        while entries.len() > self.max_entries {
            let Some(oldest_key) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            entries.remove(&oldest_key);
        }
    }
}

impl Server {
    pub fn new(
        fetcher: Arc<Fetcher>,
        auth: Arc<ResolverAuth>,
        credits: Arc<ResolverCreditsClient>,
        fallback_cache: Arc<FetchFallbackCache>,
    ) -> Self {
        Self {
            fetcher,
            auth,
            credits,
            fallback_cache,
        }
    }
}

#[tool_router]
impl Server {
    #[tool(
        name = "fetch_source",
        description = "Fetch the exact source URL supplied by the user using adaptive resolver routing. Use this whenever the prompt contains a source URL, `source: <url>`, `only take this source`, or says the URL is the source of truth. Do not perform web search or substitute another article.",
        annotations(
            title = "Fetch Source",
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        ),
        meta = "fetch_source_tool_meta()"
    )]
    async fn fetch_source(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(Params {
            url,
            idempotency_key,
            provenance,
            publish_to_arweave,
            register_onchain,
            wait_for_publication,
        }): Parameters<Params>,
    ) -> Result<CallToolResult, ErrorData> {
        let auth_context = match self.require_oauth(&parts).await? {
            Ok(context) => context,
            Err(result) => return Ok(result),
        };

        let provenance_options = ProvenanceOptions {
            provenance: provenance.unwrap_or(false),
            publish_to_arweave: publish_to_arweave.unwrap_or(false),
            register_onchain: register_onchain.unwrap_or(false),
            wait_for_publication: wait_for_publication.unwrap_or(false),
        }
        .normalized();
        if let Err(err) = validate_source_url(&url)
            .and_then(|_| validate_idempotency_key(idempotency_key.as_deref()))
            .and_then(|_| provenance_options.validate())
        {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Invalid fetch_source request: {}",
                public_validation_message(&err)
            ))]));
        }

        let started = Instant::now();
        let source_sha256 = crate::security::sensitive_hash(&url);
        eprintln!(
            "{}",
            json!({
                "event": "mcp_fetch_source_start",
                "request_id": crate::security::current_request_id(),
                "tenant_id": auth_context.tenant_id.as_deref(),
                "project_id": auth_context.project_id.as_deref(),
                "source_sha256": &source_sha256,
            })
        );
        let cache_key = normalize_fallback_url_key(&url);
        let fallback_reason = self.fallback_cache.lookup(&cache_key);
        let data = if let Some(reason) = fallback_reason {
            eprintln!(
                "{}",
                json!({
                    "event": "mcp_fetch_source_fallback",
                    "request_id": crate::security::current_request_id(),
                    "mode": "unblock",
                    "reason": reason,
                    "source_sha256": &source_sha256,
                })
            );
            self.fetcher
                .get_unblock_data_with_receipt_with_options(
                    &url,
                    Some(&auth_context),
                    provenance_options,
                )
                .await
                .map_err(|_| {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "mcp_fetch_source_failed",
                            "request_id": crate::security::current_request_id(),
                            "mode": "unblock",
                            "source_sha256": &source_sha256,
                            "elapsed_ms": started.elapsed().as_millis(),
                        })
                    );
                    ErrorData::internal_error("Upstream fetch failed", None)
                })?
        } else {
            match self
                .fetcher
                .get_fast_data_with_receipt_with_options(
                    &url,
                    Some(&auth_context),
                    provenance_options,
                )
                .await
            {
                Ok(data) => {
                    analyze_fetch_data_in_background(
                        self.fallback_cache.clone(),
                        cache_key,
                        data.data.clone(),
                    );
                    data
                }
                Err(e) => {
                    analyze_fetch_error_in_background(
                        self.fallback_cache.clone(),
                        cache_key,
                        e.to_string(),
                    );
                    eprintln!(
                        "{}",
                        json!({
                            "event": "mcp_fetch_source_failed",
                            "request_id": crate::security::current_request_id(),
                            "mode": "fast",
                            "source_sha256": &source_sha256,
                            "elapsed_ms": started.elapsed().as_millis(),
                        })
                    );
                    return Err(ErrorData::internal_error("Upstream fetch failed", None));
                }
            }
        };

        let billing_route = if fallback_reason.is_some() {
            "fetchunblock"
        } else {
            "mcp.fetch_source"
        };
        let charge = self
            .credits
            .charge_for(billing_route, started.elapsed(), provenance_options);
        match self
            .credits
            .debit_fetch_source(
                &auth_context,
                &url,
                idempotency_key.as_deref(),
                charge,
                provenance_options,
            )
            .await
        {
            Ok(Some(outcome)) => {
                eprintln!(
                    "{}",
                    json!({
                        "event": "mcp_credit_debit",
                        "request_id": crate::security::current_request_id(),
                        "source_sha256": &source_sha256,
                        "charged": outcome.charged,
                        "amount": outcome.amount,
                        "mode": outcome.mode,
                        "enforced": outcome.enforced,
                        "elapsed_ms": charge.elapsed_ms,
                        "credit_budget": charge.budget,
                    })
                );
            }
            Ok(None) => {
                eprintln!(
                    "{}",
                    json!({
                        "event": "mcp_credit_debit_skipped",
                        "request_id": crate::security::current_request_id(),
                        "source_sha256": &source_sha256,
                        "elapsed_ms": charge.elapsed_ms,
                        "computed_amount": charge.amount,
                    })
                );
            }
            Err(err) => {
                eprintln!(
                    "{}",
                    json!({
                        "event": "mcp_credit_debit_failed",
                        "request_id": crate::security::current_request_id(),
                        "source_sha256": &source_sha256,
                        "elapsed_ms": charge.elapsed_ms,
                        "computed_amount": charge.amount,
                        "payment_required": err.is_payment_required(),
                    })
                );
                let message = if err.is_payment_required() {
                    "Livy payment required".to_string()
                } else {
                    "Livy credit settlement service is unavailable".to_string()
                };
                return Ok(CallToolResult::error(vec![Content::text(message)]));
            }
        }

        let text = render_fetch_result(&data);
        eprintln!(
            "{}",
            json!({
                "event": "mcp_fetch_source_ok",
                "request_id": crate::security::current_request_id(),
                "mode": if fallback_reason.is_some() { "unblock" } else { "fast" },
                "source_sha256": &source_sha256,
                "elapsed_ms": started.elapsed().as_millis(),
                "response_bytes": text.len(),
            })
        );
        let mut result = CallToolResult::structured(structured_fetch_result(&data));
        result.content = vec![Content::text(text)];
        Ok(result)
    }
}

fn public_validation_message(error: &crate::errors::FetchError) -> &str {
    match error {
        crate::errors::FetchError::BadRequest(message) => message,
        _ => "invalid input",
    }
}

pub async fn challenge_protected_mcp_requests(
    State(auth): State<Arc<ResolverAuth>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    require_mcp_oauth_or_challenge(auth, request, next).await
}

async fn require_mcp_oauth_or_challenge(
    auth: Arc<ResolverAuth>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match auth
        .validate_authorization_header(
            request.headers().get(header::AUTHORIZATION),
            FETCH_SOURCE_SCOPES,
        )
        .await
    {
        Ok(_) => next.run(request).await,
        Err(ResolverAuthError::ServiceUnavailable(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "OAuth validation service is unavailable",
                "code": "auth_service_unavailable",
                "request_id": crate::security::current_request_id(),
            })),
        )
            .into_response(),
        Err(err) => {
            let Some((error, message)) = err.challenge_parts() else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "OAuth validation failed" })),
                )
                    .into_response();
            };
            let status = match err {
                ResolverAuthError::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
                ResolverAuthError::Forbidden { .. } => StatusCode::FORBIDDEN,
                ResolverAuthError::ServiceUnavailable(_) => unreachable!(),
            };
            mcp_http_oauth_challenge_response(&auth, status, error, message)
        }
    }
}

fn mcp_http_oauth_challenge_response(
    auth: &ResolverAuth,
    status: StatusCode,
    error: &'static str,
    message: &'static str,
) -> Response {
    let mut response = (
        status,
        Json(json!({
            "error": message,
            "code": error,
            "request_id": crate::security::current_request_id(),
        })),
    )
        .into_response();
    if let Ok(value) = auth.challenge(FETCH_SOURCE_SCOPES, error, message).parse() {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

pub async fn mirror_tools_list_security_schemes(request: Request<Body>, next: Next) -> Response {
    if request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let body_bytes = match to_bytes(body, MCP_COMPAT_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "MCP request body is too large for connector metadata compatibility handling",
            )
                .into_response();
        }
    };
    let should_mirror = is_tools_list_request(&body_bytes);
    let request = Request::from_parts(parts, Body::from(body_bytes));
    let response = next.run(request).await;

    if should_mirror {
        mirror_tools_list_response_security_schemes(response).await
    } else {
        response
    }
}

fn is_tools_list_request(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };

    match value {
        Value::Object(object) => object.get("method").and_then(Value::as_str) == Some("tools/list"),
        Value::Array(messages) => messages.into_iter().any(|message| {
            message
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| method == "tools/list")
        }),
        _ => false,
    }
}

async fn mirror_tools_list_response_security_schemes(response: Response) -> Response {
    if !response.status().is_success() {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, MCP_COMPAT_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "MCP tools/list response is too large for connector metadata compatibility handling",
            )
                .into_response();
        }
    };

    let Ok(body_text) = std::str::from_utf8(&body_bytes) else {
        return Response::from_parts(parts, Body::from(body_bytes));
    };
    let (rewritten, changed) = enrich_tools_list_response_for_chatgpt(body_text);
    if !changed {
        return Response::from_parts(parts, Body::from(body_bytes));
    }

    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(rewritten))
}

fn enrich_tools_list_response_for_chatgpt(body: &str) -> (String, bool) {
    let mut changed = false;
    let mut rewritten = String::with_capacity(body.len());

    for line in body.split_inclusive('\n') {
        let Some(rest) = line.strip_prefix("data:") else {
            rewritten.push_str(line);
            continue;
        };
        let line_ending = if rest.ends_with('\n') { "\n" } else { "" };
        let payload = rest.trim_end_matches('\n').trim_start();
        let Ok(mut value) = serde_json::from_str::<Value>(payload) else {
            rewritten.push_str(line);
            continue;
        };

        if enrich_tools_list_message_for_chatgpt(&mut value) {
            changed = true;
            rewritten.push_str("data: ");
            rewritten.push_str(&value.to_string());
            rewritten.push_str(line_ending);
        } else {
            rewritten.push_str(line);
        }
    }

    (rewritten, changed)
}

fn enrich_tools_list_message_for_chatgpt(message: &mut Value) -> bool {
    let Some(tools) = message
        .get_mut("result")
        .and_then(|result| result.get_mut("tools"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    let mut changed = false;
    for tool in tools {
        let Some(tool_object) = tool.as_object_mut() else {
            continue;
        };
        if tool_object.get("name").and_then(Value::as_str) == Some("fetch_source") {
            if !tool_object.contains_key("title") {
                tool_object.insert("title".to_string(), json!(FETCH_SOURCE_TITLE));
                changed = true;
            }
            if !tool_object.contains_key("outputSchema") {
                tool_object.insert("outputSchema".to_string(), fetch_source_output_schema());
                changed = true;
            }

            let meta = tool_object
                .entry("_meta".to_string())
                .or_insert_with(|| json!({}));
            if let Some(meta_object) = meta.as_object_mut() {
                let ui = meta_object
                    .entry("ui".to_string())
                    .or_insert_with(|| json!({}));
                if let Some(ui_object) = ui.as_object_mut() {
                    if !ui_object.contains_key("visibility") {
                        ui_object.insert("visibility".to_string(), chatgpt_tool_visibility());
                        changed = true;
                    }
                }
                if !meta_object.contains_key("securitySchemes") {
                    meta_object.insert(
                        "securitySchemes".to_string(),
                        fetch_source_security_schemes(),
                    );
                    changed = true;
                }
                if !meta_object.contains_key("openai/toolInvocation/invoking") {
                    meta_object.insert(
                        "openai/toolInvocation/invoking".to_string(),
                        json!(FETCH_SOURCE_INVOCATION_START),
                    );
                    changed = true;
                }
                if !meta_object.contains_key("openai/toolInvocation/invoked") {
                    meta_object.insert(
                        "openai/toolInvocation/invoked".to_string(),
                        json!(FETCH_SOURCE_INVOCATION_DONE),
                    );
                    changed = true;
                }
                if !meta_object.contains_key("openai/visibility") {
                    meta_object.insert("openai/visibility".to_string(), json!("public"));
                    changed = true;
                }
            }

            let annotations = tool_object
                .entry("annotations".to_string())
                .or_insert_with(|| json!({}));
            if let Some(annotation_object) = annotations.as_object_mut() {
                if !annotation_object.contains_key("title") {
                    annotation_object.insert("title".to_string(), json!(FETCH_SOURCE_TITLE));
                    changed = true;
                }
                if !annotation_object.contains_key("readOnlyHint") {
                    annotation_object.insert("readOnlyHint".to_string(), json!(true));
                    changed = true;
                }
                if !annotation_object.contains_key("destructiveHint") {
                    annotation_object.insert("destructiveHint".to_string(), json!(false));
                    changed = true;
                }
                if !annotation_object.contains_key("openWorldHint") {
                    annotation_object.insert("openWorldHint".to_string(), json!(true));
                    changed = true;
                }
            }
        }

        if tool_object.contains_key("securitySchemes") {
            continue;
        }

        let Some(schemes) = tool_object
            .get("_meta")
            .and_then(|meta| meta.get("securitySchemes"))
            .cloned()
        else {
            continue;
        };
        tool_object.insert("securitySchemes".to_string(), schemes);
        changed = true;
    }

    changed
}

impl Server {
    async fn require_oauth(
        &self,
        parts: &Parts,
    ) -> Result<Result<ResolverAuthContext, CallToolResult>, ErrorData> {
        match self
            .auth
            .validate_authorization_header(
                parts.headers.get(header::AUTHORIZATION),
                FETCH_SOURCE_SCOPES,
            )
            .await
        {
            Ok(context) => Ok(Ok(context)),
            Err(ResolverAuthError::ServiceUnavailable(_)) => Err(ErrorData::internal_error(
                "OAuth validation service is unavailable",
                None,
            )),
            Err(err) => {
                let Some((error, message)) = err.challenge_parts() else {
                    return Err(ErrorData::internal_error("OAuth validation failed", None));
                };
                Ok(Err(oauth_challenge_result(
                    &self.auth,
                    FETCH_SOURCE_SCOPES,
                    error,
                    message,
                )))
            }
        }
    }
}

fn oauth_challenge_result(
    auth: &ResolverAuth,
    required_scopes: &[&str],
    error: &'static str,
    message: &'static str,
) -> CallToolResult {
    let mut meta = Meta::new();
    meta.insert(
        "mcp/www_authenticate".to_string(),
        json!(auth.challenge(required_scopes, error, message)),
    );

    CallToolResult::error(vec![Content::text(message)]).with_meta(Some(meta))
}

fn fetch_source_tool_meta() -> Meta {
    let mut meta = Meta::new();
    meta.insert(
        "securitySchemes".to_string(),
        fetch_source_security_schemes(),
    );
    meta.insert(
        "ui".to_string(),
        json!({
            "visibility": chatgpt_tool_visibility()
        }),
    );
    meta.insert("openai/visibility".to_string(), json!("public"));
    meta.insert(
        "openai/toolInvocation/invoking".to_string(),
        json!(FETCH_SOURCE_INVOCATION_START),
    );
    meta.insert(
        "openai/toolInvocation/invoked".to_string(),
        json!(FETCH_SOURCE_INVOCATION_DONE),
    );
    meta
}

fn fetch_source_security_schemes() -> Value {
    json!([
        {
            "type": "oauth2",
            "scopes": ["tool:fetch_source"]
        }
    ])
}

fn fetch_source_output_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "receipt_id": {
                "type": "string",
                "description": "Livy resolver receipt id for this fetch."
            },
            "explorer": {
                "type": "string",
                "description": "Livy Explorer URL for this receipt."
            },
            "source_url": {
                "type": "string",
                "description": "The exact URL that was fetched."
            },
            "status": {
                "type": "integer",
                "description": "Upstream HTTP status code when available."
            },
            "fetch_elapsed_ms": {
                "type": "integer",
                "description": "Elapsed upstream fetch time in milliseconds when available."
            },
            "content_bytes": {
                "type": "integer",
                "description": "Extracted content size in bytes when available."
            },
            "provenance": {
                "type": "object",
                "description": "Livy provenance attestation and publication status when requested."
            },
            "provenance_error": {
                "type": "string",
                "description": "Non-fatal provenance or publication error when requested work failed."
            },
            "text": {
                "type": "string",
                "description": "Fetched source text or serialized upstream payload."
            }
        },
        "required": ["receipt_id", "explorer", "source_url", "text"],
        "additionalProperties": false
    })
}

fn chatgpt_tool_visibility() -> Value {
    json!(["model", "app"])
}

fn structured_fetch_result(data: &FetchWithReceipt) -> Value {
    let mut result = serde_json::Map::new();
    let receipt = &data.receipt;

    result.insert("receipt_id".to_string(), json!(data.receipt_id));
    result.insert(
        "explorer".to_string(),
        json!(explorer_url(&data.receipt_id)),
    );
    result.insert("source_url".to_string(), json!(receipt.source_url));
    if let Some(status) = receipt.status {
        result.insert("status".to_string(), json!(status));
    }
    if let Some(elapsed) = receipt.duration_elapsed_ms {
        result.insert("fetch_elapsed_ms".to_string(), json!(elapsed));
    }
    if let Some(content_bytes) = receipt.content_bytes {
        result.insert("content_bytes".to_string(), json!(content_bytes));
    }
    if let Some(provenance) = &data.provenance {
        result.insert("provenance".to_string(), json!(provenance));
    }
    if let Some(error) = &data.provenance_error {
        result.insert("provenance_error".to_string(), json!(error));
    }
    result.insert(
        "text".to_string(),
        json!(
            extracted_content(&data.data)
                .map(str::to_string)
                .unwrap_or_else(|| data.data.to_string())
        ),
    );

    Value::Object(result)
}

fn render_fetch_result(data: &FetchWithReceipt) -> String {
    let mut output = String::new();
    let receipt = &data.receipt;

    let _ = writeln!(output, "receipt_id: {}", data.receipt_id);
    let _ = writeln!(output, "explorer: {}", explorer_url(&data.receipt_id));
    let _ = writeln!(output, "status: {}", display_option(receipt.status));
    let _ = writeln!(
        output,
        "fetch_elapsed_ms: {}",
        display_option(receipt.duration_elapsed_ms)
    );
    let _ = writeln!(
        output,
        "content_bytes: {}",
        display_option(receipt.content_bytes)
    );
    if let Some(provenance) = &data.provenance {
        let _ = writeln!(
            output,
            "provenance_attestation_id: {}",
            provenance.provenance_attestation_id
        );
        if let Some(explorer) = &provenance.explorer_url {
            let _ = writeln!(output, "provenance_explorer: {explorer}");
        }
    }
    if let Some(error) = &data.provenance_error {
        let _ = writeln!(output, "provenance_error: {error}");
    }
    output.push_str("\n---\n\n");
    match extracted_content(&data.data) {
        Some(content) => output.push_str(content),
        None => output.push_str(&data.data.to_string()),
    }

    output
}

fn explorer_url(receipt_id: &str) -> String {
    format!("{EXPLORER_QUERY_BASE_URL}{receipt_id}")
}

fn display_option<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extracted_content(data: &Value) -> Option<&str> {
    if let Some(content) = data.get("content").and_then(Value::as_str) {
        return Some(content);
    }

    data.as_array()?
        .iter()
        .find_map(|item| item.get("content").and_then(Value::as_str))
}

fn analyze_fetch_data_in_background(cache: Arc<FetchFallbackCache>, key: String, data: Value) {
    tokio::spawn(async move {
        if let Some(reason) = fallback_reason_for_fetch_data(&data) {
            eprintln!(
                "{}",
                json!({
                    "event": "mcp_fetch_source_fallback_flag",
                    "reason": reason,
                    "source_sha256": crate::security::sensitive_hash(&key),
                })
            );
            cache.flag(key, reason);
        }
    });
}

fn analyze_fetch_error_in_background(cache: Arc<FetchFallbackCache>, key: String, error: String) {
    tokio::spawn(async move {
        if let Some(reason) = fallback_reason_for_text(&error) {
            eprintln!(
                "{}",
                json!({
                    "event": "mcp_fetch_source_fallback_flag",
                    "reason": reason,
                    "source_sha256": crate::security::sensitive_hash(&key),
                })
            );
            cache.flag(key, reason);
        }
    });
}

fn fallback_reason_for_fetch_data(data: &Value) -> Option<&'static str> {
    if let Some(content) = extracted_content(data) {
        return fallback_reason_for_text(content);
    }
    fallback_reason_for_text(&data.to_string())
}

fn fallback_reason_for_text(text: &str) -> Option<&'static str> {
    let normalized = normalize_detector_text(text);
    if contains_any(
        &normalized,
        &[
            "enable javascript",
            "requires javascript",
            "require javascript",
            "javascript is disabled",
            "javascript disabled",
            "please enable js",
            "turn on javascript",
            "you need javascript",
            "browser is required",
        ],
    ) {
        return Some("javascript_required");
    }

    if contains_any(
        &normalized,
        &[
            "blocked by robots",
            "disallowed by robots",
            "robots.txt",
            "robots policy",
            "respect robots",
        ],
    ) {
        return Some("robots_blocked");
    }

    if contains_any(
        &normalized,
        &[
            "verify you are human",
            "verify that you are human",
            "confirm you are human",
            "prove you are human",
            "are you a human",
            "human verification",
            "not a robot",
            "are not a robot",
            "verify you are not a robot",
            "complete the security check",
            "security check to access",
            "captcha",
        ],
    ) {
        return Some("human_verification");
    }

    if looks_like_browser_check(&normalized) {
        return Some("browser_check");
    }

    None
}

fn looks_like_browser_check(normalized: &str) -> bool {
    contains_any(
        normalized,
        &[
            "checking your browser",
            "checking if the site connection is secure",
            "just a moment...",
        ],
    ) || (normalized.contains("cloudflare")
        && contains_any(
            normalized,
            &["ray id", "attention required", "challenge", "turnstile"],
        ))
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn normalize_detector_text(text: &str) -> String {
    text.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_fallback_url_key(url: &str) -> String {
    let trimmed = url.trim();
    if let Ok(mut parsed) = reqwest::Url::parse(trimmed) {
        parsed.set_fragment(None);
        if parsed.path() == "/" && parsed.query().is_none() {
            let mut key = parsed.to_string();
            key.truncate(key.trim_end_matches('/').len());
            return key;
        }
        return parsed.to_string();
    }

    trimmed.trim_end_matches('/').to_ascii_lowercase()
}

#[tool_handler(
    name = "livygensyn-source-fetcher",
    version = "0.1.0",
    instructions = "This server only fetches user-provided source URLs through adaptive resolver routing. If a user prompt includes a URL or phrases like `source:`, `only take this source`, or `source of truth`, call `fetch_source` with that exact URL before answering. Do not search the web, do not use rumors or alternate sources, and do not replace the URL with a query."
)]
impl ServerHandler for Server {}

#[cfg(test)]
mod tests {
    use super::{
        FETCH_SOURCE_SCOPES, FallbackCacheEntry, FetchFallbackCache, Server,
        mcp_http_oauth_challenge_response, oauth_challenge_result, render_fetch_result,
        structured_fetch_result,
    };
    use crate::auth::ResolverAuth;
    use crate::types::{FetchWithReceipt, Receipt};
    use axum::{body::to_bytes, http::header};
    use serde_json::json;
    use std::time::{Duration, Instant};

    #[test]
    fn fetch_source_tool_advertises_oauth_metadata() {
        let tool = Server::fetch_source_tool_attr();
        let schemes = tool
            .meta
            .as_ref()
            .and_then(|meta| meta.get("securitySchemes"))
            .and_then(|value| value.as_array())
            .expect("securitySchemes should be present");

        assert_eq!(schemes.len(), 1);
        assert_eq!(schemes[0]["type"], "oauth2");
        assert_eq!(schemes[0]["scopes"][0], "tool:fetch_source");
        assert_eq!(
            tool.meta
                .as_ref()
                .and_then(|meta| meta.get("openai/toolInvocation/invoking")),
            Some(&json!("Fetching source"))
        );
        assert_eq!(
            tool.meta.as_ref().and_then(|meta| meta.get("ui")),
            Some(&json!({ "visibility": ["model", "app"] }))
        );
        assert_eq!(
            tool.meta
                .as_ref()
                .and_then(|meta| meta.get("openai/visibility")),
            Some(&json!("public"))
        );
        assert_eq!(
            tool.meta
                .as_ref()
                .and_then(|meta| meta.get("openai/toolInvocation/invoked")),
            Some(&json!("Source fetched"))
        );
        assert_eq!(
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations.read_only_hint),
            Some(true)
        );
        assert_eq!(
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations.destructive_hint),
            Some(false)
        );
        assert_eq!(
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations.open_world_hint),
            Some(true)
        );
    }

    #[test]
    fn oauth_challenge_result_sets_mcp_www_authenticate_meta() {
        let auth = ResolverAuth::for_tests();
        let result = oauth_challenge_result(
            &auth,
            FETCH_SOURCE_SCOPES,
            "invalid_request",
            "missing bearer token",
        );
        let challenge = result
            .meta
            .as_ref()
            .and_then(|meta| meta.get("mcp/www_authenticate"))
            .and_then(|value| value.as_str())
            .expect("MCP auth challenge should be present");

        assert_eq!(result.is_error, Some(true));
        assert!(challenge.starts_with("Bearer "));
        assert!(challenge.contains(
            "resource_metadata=\"https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource\""
        ));
        assert!(challenge.contains("scope=\"tool:fetch_source\""));
        assert!(challenge.contains("error=\"invalid_request\""));
        assert!(challenge.contains("error_description=\"missing bearer token\""));
    }

    #[tokio::test]
    async fn http_oauth_challenge_response_sets_www_authenticate_header() {
        let auth = ResolverAuth::for_tests();
        let response = mcp_http_oauth_challenge_response(
            &auth,
            axum::http::StatusCode::UNAUTHORIZED,
            "invalid_request",
            "missing bearer token",
        );

        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .expect("WWW-Authenticate header should be set");
        assert!(challenge.starts_with("Bearer "));
        assert!(challenge.contains(
            "resource_metadata=\"https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource\""
        ));
        assert!(challenge.contains("scope=\"tool:fetch_source\""));
        assert!(challenge.contains("error=\"invalid_request\""));

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(payload["error"], "missing bearer token");
    }

    #[test]
    fn detects_tools_list_requests() {
        assert!(super::is_tools_list_request(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#
        ));
        assert!(super::is_tools_list_request(
            br#"[{"jsonrpc":"2.0","id":1,"method":"initialize"},{"jsonrpc":"2.0","id":2,"method":"tools/list"}]"#
        ));
        assert!(!super::is_tools_list_request(
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{}}"#
        ));
    }

    #[test]
    fn enriches_fetch_source_descriptor_for_chatgpt() {
        let mut message = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [{
                    "name": "fetch_source",
                    "_meta": {
                        "securitySchemes": [{
                            "type": "oauth2",
                            "scopes": ["tool:fetch_source"]
                        }]
                    }
                }]
            }
        });

        assert!(super::enrich_tools_list_message_for_chatgpt(&mut message));
        assert_eq!(message["result"]["tools"][0]["title"], "Fetch Source");
        assert_eq!(
            message["result"]["tools"][0]["outputSchema"]["required"],
            json!(["receipt_id", "explorer", "source_url", "text"])
        );
        assert_eq!(
            message["result"]["tools"][0]["annotations"]["destructiveHint"],
            false
        );
        assert_eq!(
            message["result"]["tools"][0]["securitySchemes"],
            message["result"]["tools"][0]["_meta"]["securitySchemes"]
        );
        assert_eq!(
            message["result"]["tools"][0]["_meta"]["openai/toolInvocation/invoking"],
            "Fetching source"
        );
        assert_eq!(
            message["result"]["tools"][0]["_meta"]["ui"]["visibility"],
            json!(["model", "app"])
        );
        assert_eq!(
            message["result"]["tools"][0]["_meta"]["openai/visibility"],
            "public"
        );
        assert_eq!(
            message["result"]["tools"][0]["_meta"]["openai/toolInvocation/invoked"],
            "Source fetched"
        );
    }

    #[test]
    fn fetch_result_includes_receipt_explorer_url_in_all_mcp_outputs() {
        let data = FetchWithReceipt {
            receipt_id: "19f65ad0887-1".to_string(),
            receipt: Receipt {
                id: "19f65ad0887-1".to_string(),
                source_url: "https://example.com/article".to_string(),
                mode: "fast".to_string(),
                request_type: "fetch".to_string(),
                proxy: None,
                status: Some(200),
                error: None,
                duration_elapsed_ms: Some(42),
                content_bytes: Some(7),
                total_cost: None,
                created_at_unix_ms: 0,
                demo_message: String::new(),
            },
            data: json!({"content": "example"}),
            provenance: None,
            provenance_error: None,
        };

        let structured = structured_fetch_result(&data);
        assert_eq!(structured["receipt_id"], "19f65ad0887-1");
        assert_eq!(
            structured["explorer"],
            "https://explorer.livylabs.xyz/?q=19f65ad0887-1"
        );

        let rendered = render_fetch_result(&data);
        assert!(rendered.contains("receipt_id: 19f65ad0887-1\n"));
        assert!(rendered.contains("explorer: https://explorer.livylabs.xyz/?q=19f65ad0887-1\n"));
        assert!(!rendered.contains("<receipt_id>"));
    }

    #[test]
    fn fallback_detector_flags_javascript_robots_and_human_checks() {
        assert_eq!(
            super::fallback_reason_for_text("Please enable JavaScript to continue."),
            Some("javascript_required")
        );
        assert_eq!(
            super::fallback_reason_for_text("Access disallowed by robots.txt policy."),
            Some("robots_blocked")
        );
        assert_eq!(
            super::fallback_reason_for_text("Verify you are human before continuing."),
            Some("human_verification")
        );
        assert_eq!(
            super::fallback_reason_for_text("Just a moment... Checking your browser."),
            Some("browser_check")
        );
    }

    #[test]
    fn fallback_detector_ignores_normal_content() {
        assert_eq!(
            super::fallback_reason_for_text(
                "Robots are mentioned in this article, but the source content is available."
            ),
            None
        );
        assert_eq!(
            super::fallback_reason_for_fetch_data(&json!([{
                "content": "This is normal article text with a status page and useful content."
            }])),
            None
        );
    }

    #[test]
    fn fallback_detector_reads_content_from_spider_payloads() {
        assert_eq!(
            super::fallback_reason_for_fetch_data(&json!([{
                "content": "Javascript is disabled in your browser."
            }])),
            Some("javascript_required")
        );
    }

    #[test]
    fn fallback_cache_hits_expires_and_prunes_old_entries() {
        let cache = FetchFallbackCache::new(Duration::from_secs(60), 10);
        cache.flag("https://example.com/one".to_string(), "javascript_required");
        assert_eq!(
            cache.lookup("https://example.com/one"),
            Some("javascript_required")
        );

        let capped = FetchFallbackCache::new(Duration::from_secs(60), 2);
        let now = Instant::now();
        capped.entries.lock().expect("cache lock").extend([
            (
                "https://example.com/oldest".to_string(),
                FallbackCacheEntry {
                    inserted_at: now - Duration::from_secs(3),
                    reason: "javascript_required",
                },
            ),
            (
                "https://example.com/middle".to_string(),
                FallbackCacheEntry {
                    inserted_at: now - Duration::from_secs(2),
                    reason: "robots_blocked",
                },
            ),
            (
                "https://example.com/newest".to_string(),
                FallbackCacheEntry {
                    inserted_at: now - Duration::from_secs(1),
                    reason: "human_verification",
                },
            ),
        ]);
        assert_eq!(capped.lookup("https://example.com/oldest"), None);
        assert_eq!(
            capped.lookup("https://example.com/middle"),
            Some("robots_blocked")
        );
        assert_eq!(
            capped.lookup("https://example.com/newest"),
            Some("human_verification")
        );

        let expiring = FetchFallbackCache::new(Duration::from_secs(1), 10);
        expiring.entries.lock().expect("cache lock").insert(
            "https://example.com/old".to_string(),
            FallbackCacheEntry {
                inserted_at: Instant::now() - Duration::from_secs(2),
                reason: "browser_check",
            },
        );
        assert_eq!(expiring.lookup("https://example.com/old"), None);
    }

    #[test]
    fn fallback_url_key_drops_fragments_and_root_trailing_slash() {
        assert_eq!(
            super::normalize_fallback_url_key(" https://Example.com/#fragment "),
            "https://example.com"
        );
        assert_eq!(
            super::normalize_fallback_url_key("https://example.com/path/#fragment"),
            "https://example.com/path/"
        );
    }
}
