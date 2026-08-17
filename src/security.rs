//! Request correlation, safe access logs, timeouts, and response hardening.

use crate::config::{McpRuntimeConfig, SecurityConfig};
use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static REQUEST_ID: String;
}

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const MCP_CANCELLATION_GRACE: Duration = Duration::from_millis(100);

/// Cooperative cancellation inserted before RMCP detaches stateless request handling.
#[derive(Clone, Debug)]
pub struct McpRequestCancellation(pub CancellationToken);

pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(Clone::clone).ok()
}

pub fn sensitive_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub async fn request_security(
    State(config): State<SecurityConfig>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().to_string();
    let path = normalized_path(request.uri().path()).to_string();
    let request_id = incoming_request_id(&request).unwrap_or_else(new_request_id);
    request.extensions_mut().insert(request_id.clone());

    let response = REQUEST_ID
        .scope(request_id.clone(), next.run(request))
        .await;
    let status = response.status();
    let response = apply_security_headers(response, &request_id, config.hsts_enabled);

    let log = json!({
        "event": "http_request",
        "request_id": request_id,
        "method": method,
        "route": path,
        "status": status.as_u16(),
        "latency_ms": started.elapsed().as_millis(),
        "response_bytes": response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok()),
    });
    eprintln!("{log}");
    response
}

pub async fn product_timeout(
    State(config): State<SecurityConfig>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match tokio::time::timeout(config.product_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({
                "error": "Request timed out",
                "code": "request_timeout",
                "request_id": current_request_id(),
            })),
        )
            .into_response(),
    }
}

pub async fn mcp_timeout(
    State(config): State<SecurityConfig>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let cancellation = CancellationToken::new();
    request
        .extensions_mut()
        .insert(McpRequestCancellation(cancellation.clone()));
    let mut downstream = Box::pin(next.run(request));
    tokio::select! {
        response = &mut downstream => return response,
        () = tokio::time::sleep(config.mcp_timeout) => {}
    }

    cancellation.cancel();
    // Cooperative tool cancellation normally completes immediately. A stalled request body or
    // non-cooperative downstream gets only this bounded drain window before its future is dropped.
    let _ = tokio::time::timeout(MCP_CANCELLATION_GRACE, &mut downstream).await;
    drop(downstream);
    (
        StatusCode::GATEWAY_TIMEOUT,
        Json(json!({
            "error": "MCP request timed out",
            "code": "mcp_request_timeout",
            "request_id": current_request_id(),
        })),
    )
        .into_response()
}

/// RMCP accepts a configured origin with no port as a wildcard. Enforce the exact effective
/// origin tuple before the transport sees the request.
pub async fn exact_mcp_origin(
    State(config): State<McpRuntimeConfig>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(origin) = request.headers().get(header::ORIGIN) else {
        return next.run(request).await;
    };
    let allowed = origin
        .to_str()
        .ok()
        .is_some_and(|origin| config.allows_origin(origin));
    if allowed {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "MCP Origin is not allowed",
                "code": "mcp_origin_forbidden",
                "request_id": current_request_id(),
            })),
        )
            .into_response()
    }
}

fn incoming_request_id(request: &Request<Body>) -> Option<String> {
    let value = request.headers().get("x-request-id")?.to_str().ok()?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(value.to_string())
}

fn new_request_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("req-{timestamp:x}-{sequence:x}")
}

fn normalized_path(path: &str) -> &str {
    if path.starts_with("/receipt/") {
        "/receipt/{id}"
    } else {
        path
    }
}

fn apply_security_headers(mut response: Response, request_id: &str, hsts: bool) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; base-uri 'none'"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    if hsts {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert("x-request-id", value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parameters_are_not_logged() {
        assert_eq!(normalized_path("/receipt/secret"), "/receipt/{id}");
        assert_eq!(normalized_path("/fetch"), "/fetch");
    }
}
