//! Axum entrypoint that mounts HTTP product routes and MCP transport.

mod api;
mod auth;
mod config;
mod credits;
mod egress;
mod errors;
mod fetch;
mod lifecycle;
mod mcp;
mod provenance;
mod security;
mod snapshot_upload;
mod types;
use api::{
    crawl_post, extract_post, fetch_fast, fetch_post, fetch_unblock, get_receipt, map_post,
    screenshot_post, search_post, snapshot_source,
};
use axum::{
    Extension, Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use std::sync::Arc;

use crate::errors::{FetchError, Result};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let security_config = config::SecurityConfig::from_env().map_err(FetchError::Http)?;
    let mcp_runtime = config::McpRuntimeConfig::from_env().map_err(FetchError::Http)?;
    let egress_policy = Arc::new(egress::EgressPolicy::from_env().map_err(FetchError::Http)?);
    let fetcher = Arc::new(fetch::Fetcher::new());
    let mcp_fetcher = fetcher.clone();
    let resolver_auth = Arc::new(auth::ResolverAuth::from_env().map_err(FetchError::Http)?);
    let resolver_credits = Arc::new(credits::ResolverCreditsClient::from_env());
    let mcp_fallback_cache = Arc::new(mcp::FetchFallbackCache::default());
    let mcp_auth = resolver_auth.clone();
    let mcp_credits = resolver_credits.clone();
    let mcp_egress = egress_policy.clone();
    let metadata_auth = resolver_auth.clone();
    let mcp_metadata_auth = resolver_auth.clone();
    let mcp_challenge_auth = resolver_auth.clone();

    let mcp_server_config = mcp_config(&mcp_runtime);
    let mcp_cancellation = mcp_server_config.cancellation_token.clone();
    let mcp_service = StreamableHttpService::new(
        move || {
            Ok(mcp::Server::new(
                mcp_fetcher.clone(),
                mcp_auth.clone(),
                mcp_credits.clone(),
                mcp_egress.clone(),
                mcp_fallback_cache.clone(),
            ))
        },
        NeverSessionManager::default().into(),
        mcp_server_config,
    );

    let product_routes = Router::new()
        .route("/fetch", post(fetch_post))
        .route("/crawl", post(crawl_post))
        .route("/map", post(map_post))
        .route("/search", post(search_post))
        .route("/extract", post(extract_post))
        .route("/screenshot", post(screenshot_post))
        .route("/snapshot", post(snapshot_source))
        .route("/fetchfast", post(fetch_fast))
        .route("/fetchunblock", post(fetch_unblock))
        .route("/receipt/{id}", get(get_receipt))
        .route("/recipt/{id}", get(get_receipt))
        .layer(Extension(resolver_credits.clone()))
        .layer(Extension(egress_policy))
        .layer(DefaultBodyLimit::max(security_config.product_body_bytes))
        .layer(middleware::from_fn_with_state(
            security_config.clone(),
            security::product_timeout,
        ))
        .route_layer(middleware::from_fn_with_state(
            resolver_auth.clone(),
            auth::require_product_oauth,
        ));

    let mcp_routes = Router::new()
        .route_service("/", mcp_service.clone())
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(
            mcp_challenge_auth,
            mcp::challenge_protected_mcp_requests,
        ))
        .layer(middleware::from_fn(mcp::mirror_tools_list_security_schemes))
        .layer(middleware::from_fn_with_state(
            security_config.clone(),
            security::mcp_timeout,
        ));

    let runtime_state = Arc::new(lifecycle::RuntimeState::new(true));

    let app = Router::new()
        .route("/healthz", get(lifecycle::liveness))
        .route("/readyz", get(lifecycle::readiness))
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let auth = metadata_auth.clone();
                async move { auth::oauth_protected_resource_metadata(auth).await }
            }),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(move || {
                let auth = mcp_metadata_auth.clone();
                async move { auth::oauth_protected_resource_metadata(auth).await }
            }),
        )
        .merge(product_routes)
        .merge(mcp_routes)
        .layer(Extension(runtime_state.clone()))
        .layer(middleware::from_fn_with_state(
            security_config,
            security::request_security,
        ))
        .with_state(fetcher);

    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("RESOLVER_PORT"))
        .unwrap_or_else(|_| "3001".to_string());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| FetchError::Http(err.to_string()))?;
    runtime_state.mark_ready();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(runtime_state, mcp_cancellation))
        .await
        .map_err(|err| FetchError::Http(err.to_string()))?;

    Ok(())
}

fn mcp_config(runtime: &config::McpRuntimeConfig) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_allowed_hosts(runtime.allowed_hosts.clone())
        .with_allowed_origins(runtime.allowed_origins.clone())
        .with_stateful_mode(false)
        .with_json_response(true)
}

async fn shutdown_signal(
    runtime_state: Arc<lifecycle::RuntimeState>,
    mcp_cancellation: tokio_util::sync::CancellationToken,
) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            eprintln!("failed to install Ctrl+C handler: {err}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => eprintln!("failed to install SIGTERM handler: {err}"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    begin_shutdown(&runtime_state, &mcp_cancellation);
}

fn begin_shutdown(
    runtime_state: &lifecycle::RuntimeState,
    mcp_cancellation: &tokio_util::sync::CancellationToken,
) {
    runtime_state.begin_draining();
    mcp_cancellation.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rmcp::ServerHandler;
    use tower::ServiceExt;

    #[derive(Clone, Default)]
    struct TestServer;

    impl ServerHandler for TestServer {}

    #[test]
    fn mcp_transport_is_host_origin_restricted_and_stateless() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let config = mcp_config(&runtime);
        assert_eq!(config.allowed_hosts, vec!["resolver.example"]);
        assert_eq!(config.allowed_origins, vec!["https://app.example"]);
        assert!(!config.stateful_mode);
        assert!(config.json_response);
    }

    #[tokio::test]
    async fn mcp_transport_rejects_attacker_host_and_origin() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let service = StreamableHttpService::new(
            || Ok::<_, std::io::Error>(TestServer),
            NeverSessionManager::default().into(),
            mcp_config(&runtime),
        );
        let response = service
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("http://resolver.example/mcp")
                    .header("host", "attacker.example")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("service response");
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);

        let response = service
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("http://resolver.example/mcp")
                    .header("host", "resolver.example")
                    .header("origin", "https://attacker.example")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("service response");
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn graceful_shutdown_drains_readiness_and_cancels_mcp() {
        let state = Arc::new(lifecycle::RuntimeState::new(true));
        state.mark_ready();
        let cancellation = tokio_util::sync::CancellationToken::new();

        begin_shutdown(&state, &cancellation);

        assert!(cancellation.is_cancelled());
        assert_eq!(
            lifecycle::readiness(Extension(state)).await.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
