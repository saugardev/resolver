//! Axum entrypoint that mounts HTTP product routes and MCP transport.

mod api;
mod auth;
mod config;
mod credits;
mod errors;
mod fetch;
mod mcp;
mod provenance;
mod receipt_store;
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
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let security_config = config::SecurityConfig::from_env().map_err(FetchError::Http)?;
    let receipt_store_factory = receipt_store::EnvironmentReceiptStoreFactory;
    let fetcher = Arc::new(fetch::Fetcher::from_receipt_store_factory(
        &receipt_store_factory,
    )?);
    let mcp_fetcher = fetcher.clone();
    let resolver_auth = Arc::new(auth::ResolverAuth::from_env());
    let resolver_credits = Arc::new(credits::ResolverCreditsClient::from_env());
    let mcp_fallback_cache = Arc::new(mcp::FetchFallbackCache::default());
    let mcp_auth = resolver_auth.clone();
    let mcp_credits = resolver_credits.clone();
    let metadata_auth = resolver_auth.clone();
    let mcp_metadata_auth = resolver_auth.clone();
    let mcp_challenge_auth = resolver_auth.clone();

    let mcp_service = StreamableHttpService::new(
        move || {
            Ok(mcp::Server::new(
                mcp_fetcher.clone(),
                mcp_auth.clone(),
                mcp_credits.clone(),
                mcp_fallback_cache.clone(),
            ))
        },
        LocalSessionManager::default().into(),
        mcp_config(),
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
        .layer(middleware::from_fn(mcp::mirror_tools_list_security_schemes));

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
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

    axum::serve(listener, app)
        .await
        .map_err(|err| FetchError::Http(err.to_string()))?;

    Ok(())
}

fn mcp_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default().disable_allowed_hosts()
}
