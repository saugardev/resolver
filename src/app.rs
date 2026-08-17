//! Public resolver application construction and runtime seams.

use crate::api::{
    crawl_post, extract_post, fetch_fast, fetch_post, fetch_unblock, get_receipt, map_post,
    screenshot_post, search_post, snapshot_source,
};
use crate::auth::{self, ResolverAuth};
use crate::config::SecurityConfig;
use crate::credits::ResolverCreditsClient;
use crate::errors::{FetchError, Result};
use crate::fetch::Fetcher;
use crate::mcp::{self, FetchFallbackCache};
use crate::receipt_store::{ReceiptStore, ReceiptStoreFactory};
use crate::security;
use axum::{
    Extension, Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use std::sync::Arc;

#[derive(Clone)]
struct ReceiptReadiness(Arc<dyn ReceiptStore>);

/// Build the complete resolver router with an application-supplied receipt store factory.
pub async fn build_app_with_receipt_store_factory(
    factory: &dyn ReceiptStoreFactory,
) -> Result<Router> {
    dotenvy::dotenv().ok();
    let security_config = SecurityConfig::from_env().map_err(FetchError::Http)?;
    let receipts = factory
        .create()
        .await
        .map_err(|err| FetchError::Http(format!("receipt store initialization failed: {err}")))?;
    let readiness = ReceiptReadiness(receipts.clone());
    let fetcher = Arc::new(Fetcher::with_receipt_store(receipts).await?);
    let mcp_fetcher = fetcher.clone();
    let resolver_auth = Arc::new(ResolverAuth::from_env());
    let resolver_credits = Arc::new(ResolverCreditsClient::from_env());
    let mcp_fallback_cache = Arc::new(FetchFallbackCache::default());
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

    Ok(Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(receipt_readiness))
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
        .layer(Extension(readiness))
        .layer(middleware::from_fn_with_state(
            security_config,
            security::request_security,
        ))
        .with_state(fetcher))
}

/// Build and serve the resolver with an application-supplied receipt store factory.
pub async fn run_with_receipt_store_factory(factory: &dyn ReceiptStoreFactory) -> Result<()> {
    let app = build_app_with_receipt_store_factory(factory).await?;
    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("RESOLVER_PORT"))
        .unwrap_or_else(|_| "3001".to_string());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| FetchError::Http(err.to_string()))?;

    axum::serve(listener, app)
        .await
        .map_err(|err| FetchError::Http(err.to_string()))
}

async fn receipt_readiness(Extension(readiness): Extension<ReceiptReadiness>) -> Response {
    match readiness.0.health_check().await {
        Ok(()) => (StatusCode::OK, "ready").into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "Receipt storage is unavailable",
                "code": "receipt_store_unavailable",
            })),
        )
            .into_response(),
    }
}

fn mcp_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default().disable_allowed_hosts()
}

#[cfg(test)]
mod tests {
    use super::{ReceiptReadiness, build_app_with_receipt_store_factory, receipt_readiness};
    use crate::FetchError;
    use crate::receipt_store::{
        ReceiptOwner, ReceiptStore, ReceiptStoreError, ReceiptStoreFactory,
    };
    use crate::types::Receipt;
    use async_trait::async_trait;
    use axum::{Extension, http::StatusCode};
    use std::sync::Arc;

    struct UnhealthyStore;

    #[async_trait]
    impl ReceiptStore for UnhealthyStore {
        async fn put(
            &self,
            _owner: &ReceiptOwner,
            _receipt: Receipt,
        ) -> Result<(), ReceiptStoreError> {
            Err(ReceiptStoreError::InvalidConfiguration(
                "remote store unavailable".to_string(),
            ))
        }

        async fn get(
            &self,
            _owner: &ReceiptOwner,
            _receipt_id: &str,
        ) -> Result<Option<Receipt>, ReceiptStoreError> {
            Err(ReceiptStoreError::InvalidConfiguration(
                "remote store unavailable".to_string(),
            ))
        }

        fn is_shared_durable(&self) -> bool {
            true
        }

        async fn health_check(&self) -> Result<(), ReceiptStoreError> {
            Err(ReceiptStoreError::InvalidConfiguration(
                "remote store unavailable".to_string(),
            ))
        }
    }

    struct ExternalFactory {
        store: Arc<dyn ReceiptStore>,
    }

    #[async_trait]
    impl ReceiptStoreFactory for ExternalFactory {
        async fn create(&self) -> Result<Arc<dyn ReceiptStore>, ReceiptStoreError> {
            Ok(self.store.clone())
        }
    }

    struct FailingFactory;

    #[async_trait]
    impl ReceiptStoreFactory for FailingFactory {
        async fn create(&self) -> Result<Arc<dyn ReceiptStore>, ReceiptStoreError> {
            Err(ReceiptStoreError::InvalidConfiguration(
                "remote initialization failed".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn unhealthy_external_store_fails_startup_and_readiness() {
        let store: Arc<dyn ReceiptStore> = Arc::new(UnhealthyStore);
        let factory = ExternalFactory {
            store: store.clone(),
        };

        let startup = build_app_with_receipt_store_factory(&factory).await;
        assert!(matches!(
            startup,
            Err(FetchError::Http(message)) if message.contains("receipt store health check failed")
        ));

        let response = receipt_readiness(Extension(ReceiptReadiness(store))).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn external_factory_initialization_failure_is_fail_closed() {
        assert!(matches!(
            build_app_with_receipt_store_factory(&FailingFactory).await,
            Err(FetchError::Http(message)) if message.contains("receipt store initialization failed")
        ));
    }
}
