//! Livy OAuth bearer-token enforcement for product routes and MCP tool calls.

use crate::errors::ResolverAuthError;
use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_INTROSPECTION_URL: &str = "https://auth.livylabs.xyz/oauth/introspect";
const DEFAULT_AUTHORIZATION_SERVER: &str = "https://auth.livylabs.xyz";
const DEFAULT_RESOLVER_AUDIENCE: &str = "https://resolver.api.livylabs.xyz/mcp";
const LEGACY_RESOLVER_AUDIENCE: &str = "https://resolver.api.livylabs.xyz";

#[derive(Clone)]
pub struct ResolverAuth {
    enabled: bool,
    client: reqwest::Client,
    introspection_url: String,
    authorization_server: String,
    audience: String,
    accepted_audiences: Vec<String>,
    resource_metadata_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverAuthContext {
    pub access_token: Option<String>,
    /// Stable OAuth resource-owner subject (`sub`) from token introspection.
    pub subject: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
    pub audiences: Vec<String>,
    pub tenant_id: Option<String>,
    pub project_id: Option<String>,
}

impl ResolverAuthContext {
    fn local_dev() -> Self {
        Self {
            access_token: None,
            subject: None,
            client_id: None,
            scopes: vec!["*".to_string()],
            audiences: Vec::new(),
            tenant_id: None,
            project_id: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    aud: Option<AudienceValue>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(rename = "https://claims.livylabs.xyz/tenant_id", default)]
    tenant_id: Option<String>,
    #[serde(rename = "https://claims.livylabs.xyz/project_id", default)]
    project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AudienceValue {
    One(String),
    Many(Vec<String>),
}

impl ResolverAuth {
    pub fn from_env() -> Self {
        let audience = normalize_url_identifier(
            env_value("LIVY_RESOLVER_OAUTH_AUDIENCE")
                .or_else(|| env_value("RWA_RESOLVER_OAUTH_AUDIENCE"))
                .unwrap_or_else(|| DEFAULT_RESOLVER_AUDIENCE.to_string()),
        );
        let resource_metadata_url = normalize_url_identifier(
            env_value("LIVY_RESOLVER_OAUTH_RESOURCE_METADATA_URL")
                .or_else(|| env_value("RWA_RESOLVER_OAUTH_RESOURCE_METADATA_URL"))
                .unwrap_or_else(|| {
                    format!(
                        "{}/.well-known/oauth-protected-resource",
                        LEGACY_RESOLVER_AUDIENCE
                    )
                }),
        );
        let accepted_audiences = accepted_audiences(&audience);

        Self {
            enabled: auth_enabled(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("resolver auth HTTP client should initialize"),
            introspection_url: normalize_url_identifier(
                env_value("LIVY_OAUTH_INTROSPECTION_URL")
                    .or_else(|| env_value("RWA_OAUTH_INTROSPECTION_URL"))
                    .unwrap_or_else(|| DEFAULT_INTROSPECTION_URL.to_string()),
            ),
            authorization_server: normalize_url_identifier(
                env_value("LIVY_OAUTH_ISSUER")
                    .or_else(|| env_value("RWA_OAUTH_ISSUER"))
                    .unwrap_or_else(|| DEFAULT_AUTHORIZATION_SERVER.to_string()),
            ),
            audience,
            accepted_audiences,
            resource_metadata_url,
        }
    }

    async fn introspect(&self, token: &str) -> Result<IntrospectionResponse, String> {
        let response = self
            .client
            .post(&self.introspection_url)
            .form(&[("token", token)])
            .send()
            .await
            .map_err(|err| format!("OAuth introspection failed: {err}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "OAuth introspection returned {}",
                response.status()
            ));
        }

        response
            .json::<IntrospectionResponse>()
            .await
            .map_err(|err| format!("OAuth introspection response is invalid: {err}"))
    }

    fn audience_allowed(&self, audiences: &[String]) -> bool {
        if self.accepted_audiences.is_empty() {
            return true;
        }
        audiences
            .iter()
            .any(|audience| self.accepted_audiences.contains(audience))
    }

    pub fn challenge(&self, required_scopes: &[&str], error: &str, description: &str) -> String {
        let scope = required_scopes
            .first()
            .copied()
            .unwrap_or("tool:fetch_source");
        format!(
            "Bearer resource_metadata=\"{}\", scope=\"{}\", error=\"{}\", error_description=\"{}\"",
            header_quote(&self.resource_metadata_url),
            header_quote(scope),
            header_quote(error),
            header_quote(description)
        )
    }

    pub async fn validate_authorization_header(
        &self,
        authorization: Option<&HeaderValue>,
        required_scopes: &[&str],
    ) -> Result<ResolverAuthContext, ResolverAuthError> {
        if !self.enabled {
            return Ok(ResolverAuthContext::local_dev());
        }

        let Some(token) = authorization.and_then(bearer_token_from_header) else {
            return Err(ResolverAuthError::Unauthorized {
                error: "invalid_request",
                message: "missing bearer token",
            });
        };
        let token = token.to_string();

        let introspection = match self.introspect(&token).await {
            Ok(response) if response.active => response,
            Ok(_) => {
                return Err(ResolverAuthError::Unauthorized {
                    error: "invalid_token",
                    message: "inactive bearer token",
                });
            }
            Err(err) => return Err(ResolverAuthError::ServiceUnavailable(err)),
        };

        let audiences = audience_values(introspection.aud.as_ref());
        if !self.audience_allowed(&audiences) {
            return Err(ResolverAuthError::Unauthorized {
                error: "invalid_token",
                message: "bearer token audience is not valid for this resolver",
            });
        }

        let scopes = parse_scope_list(&introspection.scope);
        if !required_scopes
            .iter()
            .any(|required_scope| scope_allows(&scopes, required_scope))
        {
            return Err(ResolverAuthError::Forbidden {
                error: "insufficient_scope",
                message: "bearer token is missing the required resolver scope",
            });
        }

        let tenant_id = non_empty(introspection.tenant_id);
        if tenant_id.is_none() {
            return Err(ResolverAuthError::Unauthorized {
                error: "invalid_token",
                message: "bearer token is missing Livy tenant claim",
            });
        }
        let project_id = non_empty(introspection.project_id);
        if project_id.is_none() {
            return Err(ResolverAuthError::Unauthorized {
                error: "invalid_token",
                message: "bearer token is missing Livy project claim",
            });
        }
        let subject = non_empty(introspection.sub);
        if subject.is_none() {
            return Err(ResolverAuthError::Unauthorized {
                error: "invalid_token",
                message: "bearer token is missing OAuth subject claim",
            });
        }

        Ok(ResolverAuthContext {
            access_token: Some(token),
            subject,
            client_id: non_empty(introspection.client_id),
            scopes: scopes.into_iter().map(str::to_string).collect(),
            audiences,
            tenant_id,
            project_id,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            enabled: true,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .expect("client"),
            introspection_url: "https://auth.livylabs.xyz/oauth/introspect".to_string(),
            authorization_server: "https://auth.livylabs.xyz".to_string(),
            audience: "https://resolver.api.livylabs.xyz/mcp".to_string(),
            accepted_audiences: vec![
                "https://resolver.api.livylabs.xyz/mcp".to_string(),
                "https://resolver.api.livylabs.xyz".to_string(),
            ],
            resource_metadata_url:
                "https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource".to_string(),
        }
    }
}

pub async fn oauth_protected_resource_metadata(auth: Arc<ResolverAuth>) -> Json<serde_json::Value> {
    Json(json!({
        "resource": auth.audience,
        "resource_name": "Livy Resolver",
        "authorization_servers": [auth.authorization_server],
        "introspection_endpoint": auth.introspection_url,
        "scopes_supported": resolver_scopes_supported(),
        "bearer_methods_supported": ["header"]
    }))
}

pub async fn require_product_oauth(
    State(auth): State<Arc<ResolverAuth>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let required_scopes = product_required_scopes(request.uri().path());
    require_oauth(auth, request, next, required_scopes).await
}

async fn require_oauth(
    auth: Arc<ResolverAuth>,
    request: Request<Body>,
    next: Next,
    required_scopes: &[&str],
) -> Response {
    match auth
        .validate_authorization_header(
            request.headers().get(header::AUTHORIZATION),
            required_scopes,
        )
        .await
    {
        Ok(context) => {
            let mut request = request;
            request.extensions_mut().insert(context);
            next.run(request).await
        }
        Err(ResolverAuthError::Unauthorized { error, message }) => {
            unauthorized(&auth, required_scopes, error, message)
        }
        Err(ResolverAuthError::Forbidden { error, message }) => {
            forbidden(&auth, required_scopes, error, message)
        }
        Err(ResolverAuthError::ServiceUnavailable(err)) => auth_service_unavailable(err),
    }
}

fn product_required_scopes(path: &str) -> &'static [&'static str] {
    match path {
        "/crawl" => &["resolver:source:crawl"],
        "/map" => &["resolver:source:map"],
        "/search" => &["resolver:source:search"],
        "/extract" => &["resolver:source:extract"],
        "/screenshot" => &["resolver:source:screenshot"],
        "/snapshot" => &["resolver:snapshot:create"],
        path if path.starts_with("/receipt/") || path.starts_with("/recipt/") => {
            &["resolver:receipt:read"]
        }
        _ => &["resolver:source:fetch"],
    }
}

fn bearer_token_from_header(value: &HeaderValue) -> Option<&str> {
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let token = token.trim();
        if !token.is_empty() && token.len() <= 16 * 1024 {
            return Some(token);
        }
    }
    None
}

fn parse_scope_list(scope: &str) -> Vec<&str> {
    scope.split_ascii_whitespace().collect()
}

fn scope_allows(granted_scopes: &[&str], required_scope: &str) -> bool {
    granted_scopes.iter().any(|scope| {
        *scope == "*"
            || *scope == required_scope
            || *scope == "resolver:*"
            || (*scope == "resolver" && required_scope.starts_with("resolver:"))
            || (*scope == "mcp" && required_scope.starts_with("tool:"))
    })
}

fn audience_values(value: Option<&AudienceValue>) -> Vec<String> {
    match value {
        Some(AudienceValue::One(audience)) => vec![audience.clone()],
        Some(AudienceValue::Many(audiences)) => audiences.clone(),
        None => Vec::new(),
    }
}

fn accepted_audiences(primary: &str) -> Vec<String> {
    let mut audiences = vec![primary.to_string()];
    if let Some(value) = env_value("LIVY_RESOLVER_OAUTH_ACCEPTED_AUDIENCES")
        .or_else(|| env_value("RWA_RESOLVER_OAUTH_ACCEPTED_AUDIENCES"))
    {
        audiences.extend(
            parse_space_or_comma_list(&value)
                .into_iter()
                .map(normalize_url_identifier),
        );
    } else {
        audiences.push(LEGACY_RESOLVER_AUDIENCE.to_string());
    }
    normalize_unique_list(audiences)
}

fn resolver_scopes_supported() -> Vec<&'static str> {
    vec![
        "mcp",
        "tool:fetch_source",
        "resolver:mcp:tools:list",
        "resolver:source:fetch",
        "resolver:source:crawl",
        "resolver:source:map",
        "resolver:source:search",
        "resolver:source:extract",
        "resolver:source:screenshot",
        "resolver:snapshot:create",
        "resolver:receipt:read",
    ]
}

fn auth_enabled() -> bool {
    let value =
        env_value("LIVY_RESOLVER_AUTH_ENABLED").or_else(|| env_value("RWA_RESOLVER_AUTH_ENABLED"));

    !matches!(
        value.as_deref().map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_url_identifier(value: String) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn parse_space_or_comma_list(value: &str) -> Vec<String> {
    value
        .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize_unique_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values.into_iter().map(normalize_url_identifier) {
        if !value.is_empty() && !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn unauthorized(
    auth: &ResolverAuth,
    required_scopes: &[&str],
    error: &'static str,
    message: &'static str,
) -> Response {
    auth_error_response(
        StatusCode::UNAUTHORIZED,
        auth,
        required_scopes,
        error,
        message,
    )
}

fn forbidden(
    auth: &ResolverAuth,
    required_scopes: &[&str],
    error: &'static str,
    message: &'static str,
) -> Response {
    auth_error_response(StatusCode::FORBIDDEN, auth, required_scopes, error, message)
}

fn auth_error_response(
    status: StatusCode,
    auth: &ResolverAuth,
    required_scopes: &[&str],
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
    if let Ok(value) = HeaderValue::from_str(&auth.challenge(required_scopes, error, message)) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

fn auth_service_unavailable(_message: String) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "OAuth validation service is unavailable",
            "code": "auth_service_unavailable",
            "request_id": crate::security::current_request_id(),
        })),
    )
        .into_response()
}

fn header_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::{
        IntrospectionResponse, ResolverAuth, ResolverAuthError, audience_values, header_quote,
        normalize_url_identifier, oauth_protected_resource_metadata,
    };
    use axum::{Json, Router, routing::post};
    use reqwest::header::HeaderValue;
    use serde_json::json;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn normalize_url_identifier_trims_trailing_slashes() {
        assert_eq!(
            normalize_url_identifier("https://auth.livylabs.xyz/".to_string()),
            "https://auth.livylabs.xyz"
        );
        assert_eq!(
            normalize_url_identifier(" https://resolver.api.livylabs.xyz/// ".to_string()),
            "https://resolver.api.livylabs.xyz"
        );
    }

    #[test]
    fn header_quote_escapes_bearer_challenge_values() {
        assert_eq!(header_quote(r#"bad"value\here"#), r#"bad\"value\\here"#);
    }

    #[test]
    fn bearer_challenge_includes_metadata_scope_and_error() {
        let auth = ResolverAuth {
            enabled: true,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .expect("client"),
            introspection_url: "https://auth.livylabs.xyz/oauth/introspect".to_string(),
            authorization_server: "https://auth.livylabs.xyz".to_string(),
            audience: "https://resolver.api.livylabs.xyz/mcp".to_string(),
            accepted_audiences: vec![
                "https://resolver.api.livylabs.xyz/mcp".to_string(),
                "https://resolver.api.livylabs.xyz".to_string(),
            ],
            resource_metadata_url:
                "https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource".to_string(),
        };
        let challenge = auth.challenge(
            &["tool:fetch_source"],
            "invalid_request",
            "missing bearer token",
        );

        assert!(challenge.starts_with("Bearer "));
        assert!(challenge.contains(
            "resource_metadata=\"https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource\""
        ));
        assert!(challenge.contains("scope=\"tool:fetch_source\""));
        assert!(challenge.contains("error=\"invalid_request\""));
        assert!(challenge.contains("error_description=\"missing bearer token\""));
    }

    #[tokio::test]
    async fn protected_resource_metadata_includes_display_name_and_introspection_endpoint() {
        let auth = ResolverAuth {
            enabled: true,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .expect("client"),
            introspection_url: "https://auth.livylabs.xyz/oauth/introspect".to_string(),
            authorization_server: "https://auth.livylabs.xyz".to_string(),
            audience: "https://resolver.api.livylabs.xyz/mcp".to_string(),
            accepted_audiences: vec![
                "https://resolver.api.livylabs.xyz/mcp".to_string(),
                "https://resolver.api.livylabs.xyz".to_string(),
            ],
            resource_metadata_url:
                "https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource".to_string(),
        };

        let axum::Json(metadata) = oauth_protected_resource_metadata(Arc::new(auth)).await;

        assert_eq!(metadata["resource_name"], "Livy Resolver");
        assert_eq!(
            metadata["resource"],
            "https://resolver.api.livylabs.xyz/mcp"
        );
        assert_eq!(
            metadata["introspection_endpoint"],
            "https://auth.livylabs.xyz/oauth/introspect"
        );
        assert_eq!(
            metadata["bearer_methods_supported"],
            serde_json::json!(["header"])
        );
    }

    #[test]
    fn audience_check_accepts_current_and_legacy_resolver_audiences() {
        let auth = ResolverAuth {
            enabled: true,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .expect("client"),
            introspection_url: "https://auth.livylabs.xyz/oauth/introspect".to_string(),
            authorization_server: "https://auth.livylabs.xyz".to_string(),
            audience: "https://resolver.api.livylabs.xyz/mcp".to_string(),
            accepted_audiences: vec![
                "https://resolver.api.livylabs.xyz/mcp".to_string(),
                "https://resolver.api.livylabs.xyz".to_string(),
            ],
            resource_metadata_url:
                "https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource".to_string(),
        };

        assert!(auth.audience_allowed(&["https://resolver.api.livylabs.xyz/mcp".to_string()]));
        assert!(auth.audience_allowed(&["https://resolver.api.livylabs.xyz".to_string()]));
        assert!(!auth.audience_allowed(&["https://api.livylabs.xyz".to_string()]));
    }

    #[test]
    fn introspection_response_parses_livy_tenant_project_claims() {
        let response: IntrospectionResponse = serde_json::from_value(json!({
            "active": true,
            "sub": "user-a",
            "scope": "openid mcp tool:fetch_source",
            "aud": ["https://resolver.api.livylabs.xyz"],
            "client_id": "chatgpt-client",
            "https://claims.livylabs.xyz/tenant_id": "tenant-a",
            "https://claims.livylabs.xyz/project_id": "project-a"
        }))
        .expect("introspection response should parse");

        assert_eq!(response.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(response.project_id.as_deref(), Some("project-a"));
        assert_eq!(response.sub.as_deref(), Some("user-a"));
        assert_eq!(
            audience_values(response.aud.as_ref()),
            vec!["https://resolver.api.livylabs.xyz".to_string()]
        );
    }

    #[tokio::test]
    async fn active_introspection_without_subject_is_rejected() {
        let app = Router::new().route(
            "/introspect",
            post(|| async {
                Json(json!({
                    "active": true,
                    "scope": "tool:fetch_source",
                    "aud": "https://resolver.api.livylabs.xyz/mcp",
                    "client_id": "client-a",
                    "https://claims.livylabs.xyz/tenant_id": "tenant-a",
                    "https://claims.livylabs.xyz/project_id": "project-a"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let auth = ResolverAuth {
            enabled: true,
            client: reqwest::Client::new(),
            introspection_url: format!("http://{address}/introspect"),
            authorization_server: "https://auth.livylabs.xyz".to_string(),
            audience: "https://resolver.api.livylabs.xyz/mcp".to_string(),
            accepted_audiences: vec!["https://resolver.api.livylabs.xyz/mcp".to_string()],
            resource_metadata_url:
                "https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource".to_string(),
        };

        let result = auth
            .validate_authorization_header(
                Some(&HeaderValue::from_static("Bearer token")),
                &["tool:fetch_source"],
            )
            .await;
        assert!(matches!(
            result,
            Err(ResolverAuthError::Unauthorized {
                error: "invalid_token",
                message: "bearer token is missing OAuth subject claim",
            })
        ));

        server.abort();
    }
}
