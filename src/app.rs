//! Public resolver application construction and runtime seams.

use crate::api::{
    crawl_post, extract_post, fetch_fast, fetch_post, fetch_unblock, get_receipt, map_post,
    screenshot_post, search_post, snapshot_source,
};
use crate::auth::{self, ResolverAuth};
use crate::config::{McpRuntimeConfig, SecurityConfig};
use crate::credits::ResolverCreditsClient;
use crate::egress::EgressPolicy;
use crate::errors::{FetchError, Result};
use crate::fetch::Fetcher;
use crate::lifecycle::{self, RuntimeState};
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
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use std::{future::IntoFuture, sync::Arc};

#[derive(Clone)]
struct ReceiptReadiness(Arc<dyn ReceiptStore>);

struct BuiltApp {
    router: Router,
    runtime_state: Arc<RuntimeState>,
    mcp_cancellation: tokio_util::sync::CancellationToken,
    shutdown_grace: std::time::Duration,
}

/// Build the complete resolver router with an application-supplied receipt store factory.
pub async fn build_app_with_receipt_store_factory(
    factory: &dyn ReceiptStoreFactory,
) -> Result<Router> {
    let built = build_runtime(factory).await?;
    built.runtime_state.mark_ready();
    Ok(built.router)
}

async fn build_runtime(factory: &dyn ReceiptStoreFactory) -> Result<BuiltApp> {
    dotenvy::dotenv().ok();
    let security_config = SecurityConfig::from_env().map_err(FetchError::Http)?;
    let shutdown_grace = security_config.shutdown_grace;
    let mcp_runtime = McpRuntimeConfig::from_env().map_err(FetchError::Http)?;
    let egress_policy = Arc::new(EgressPolicy::from_env().map_err(FetchError::Http)?);
    let receipts = factory
        .create()
        .await
        .map_err(|err| FetchError::Http(format!("receipt store initialization failed: {err}")))?;
    let readiness = ReceiptReadiness(receipts.clone());
    let fetcher = Arc::new(Fetcher::with_receipt_store(receipts).await?);
    let mcp_fetcher = fetcher.clone();
    let resolver_auth = Arc::new(ResolverAuth::from_env().map_err(FetchError::Http)?);
    let resolver_credits = Arc::new(ResolverCreditsClient::from_env());
    let mcp_fallback_cache = Arc::new(FetchFallbackCache::default());
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
        .layer(Extension(egress_policy.clone()))
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
            mcp_runtime,
            security::exact_mcp_origin,
        ))
        .layer(middleware::from_fn_with_state(
            security_config.clone(),
            security::mcp_timeout,
        ));

    let runtime_state = Arc::new(RuntimeState::new());

    let router = Router::new()
        .route("/healthz", get(lifecycle::liveness))
        .route("/readyz", get(application_readiness))
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
        .layer(Extension(runtime_state.clone()))
        .layer(Extension(egress_policy))
        .layer(middleware::from_fn_with_state(
            security_config,
            security::request_security,
        ))
        .with_state(fetcher);

    Ok(BuiltApp {
        router,
        runtime_state,
        mcp_cancellation,
        shutdown_grace,
    })
}

/// Build and serve the resolver with an application-supplied receipt store factory.
pub async fn run_with_receipt_store_factory(factory: &dyn ReceiptStoreFactory) -> Result<()> {
    let built = build_runtime(factory).await?;
    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("RESOLVER_PORT"))
        .unwrap_or_else(|_| "3001".to_string());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| FetchError::Http(err.to_string()))?;
    built.runtime_state.mark_ready();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, built.router)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result.map_err(|err| FetchError::Http(err.to_string()))?;
        }
        () = shutdown_signal() => {
            begin_shutdown(&built.runtime_state, &built.mcp_cancellation);
            let _ = shutdown_tx.send(());
            tokio::time::timeout(built.shutdown_grace, &mut server)
                .await
                .map_err(|_| FetchError::Timeout(format!(
                    "graceful shutdown exceeded {} seconds",
                    built.shutdown_grace.as_secs()
                )))?
                .map_err(|err| FetchError::Http(err.to_string()))?;
        }
    }

    Ok(())
}

async fn application_readiness(
    Extension(receipts): Extension<ReceiptReadiness>,
    Extension(runtime): Extension<Arc<RuntimeState>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
) -> Response {
    if receipts.0.health_check().await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "Receipt storage is unavailable",
                "code": "receipt_store_unavailable",
            })),
        )
            .into_response();
    }
    lifecycle::readiness(Extension(runtime), Extension(egress)).await
}

fn mcp_config(runtime: &McpRuntimeConfig) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_allowed_hosts(runtime.allowed_hosts.clone())
        .with_allowed_origins(runtime.allowed_origins.clone())
        .with_legacy_session_mode(false)
        .with_json_response(true)
}

async fn shutdown_signal() {
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
}

fn begin_shutdown(
    runtime_state: &RuntimeState,
    mcp_cancellation: &tokio_util::sync::CancellationToken,
) {
    runtime_state.begin_draining();
    mcp_cancellation.cancel();
}

#[cfg(test)]
mod tests {
    use super::{ReceiptReadiness, application_readiness, build_app_with_receipt_store_factory};
    use crate::FetchError;
    use crate::egress::EgressPolicy;
    use crate::lifecycle::RuntimeState;
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

        let runtime = Arc::new(RuntimeState::new());
        runtime.mark_ready();
        let response = application_readiness(
            Extension(ReceiptReadiness(store)),
            Extension(runtime),
            Extension(Arc::new(EgressPolicy::for_tests(&[], true))),
        )
        .await;
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

#[cfg(test)]
mod mcp_runtime_tests {
    use super::{begin_shutdown, mcp_config};
    use crate::{config, egress, lifecycle, mcp, security};
    use axum::{
        Extension, Router,
        body::{Body, to_bytes},
        http::Request,
        middleware,
    };
    use rmcp::{
        ErrorData, ServerHandler,
        handler::server::{router::tool::ToolRouter, tool::Extension as McpExtension},
        model::{CallToolResult, ContentBlock},
        tool, tool_handler, tool_router,
        transport::streamable_http_server::{
            StreamableHttpService, session::never::NeverSessionManager,
        },
    };
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::Poll,
        time::{Duration, Instant},
    };
    use tower::ServiceExt;

    type ToolResult<T, E> = std::result::Result<T, E>;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[derive(Clone, Default)]
    struct TestServer;

    impl ServerHandler for TestServer {}

    #[derive(Debug, Clone)]
    struct TestToolServer {
        #[expect(dead_code, reason = "tool_handler macro accesses this router field")]
        tool_router: ToolRouter<Self>,
        cancelled: Arc<AtomicBool>,
        evidence: Arc<AtomicUsize>,
    }

    impl TestToolServer {
        fn new(cancelled: Arc<AtomicBool>, evidence: Arc<AtomicUsize>) -> Self {
            Self {
                tool_router: Self::tool_router(),
                cancelled,
                evidence,
            }
        }
    }

    #[tool_router]
    impl TestToolServer {
        #[tool(
            name = "fetch_source",
            description = "Slow upstream test tool",
            annotations(
                title = "Fetch Source",
                read_only_hint = false,
                destructive_hint = true,
                open_world_hint = true
            )
        )]
        async fn fetch_source(
            &self,
            McpExtension(parts): McpExtension<axum::http::request::Parts>,
            rmcp_cancellation: tokio_util::sync::CancellationToken,
        ) -> ToolResult<CallToolResult, ErrorData> {
            let deadline_cancellation = parts
                .extensions
                .get::<security::McpRequestCancellation>()
                .map(|cancellation| cancellation.0.clone())
                .unwrap_or_default();
            tokio::select! {
                () = deadline_cancellation.cancelled() => {
                    self.cancelled.store(true, Ordering::Release);
                    Err(ErrorData::internal_error("cancelled", None))
                }
                () = rmcp_cancellation.cancelled() => {
                    self.cancelled.store(true, Ordering::Release);
                    Err(ErrorData::internal_error("cancelled", None))
                }
                () = tokio::time::sleep(Duration::from_millis(250)) => {
                    self.evidence.fetch_add(1, Ordering::AcqRel);
                    Ok(CallToolResult::success(vec![ContentBlock::text("finished")]))
                }
            }
        }
    }

    #[tool_handler]
    impl ServerHandler for TestToolServer {}

    #[test]
    fn mcp_transport_is_host_origin_restricted_and_stateless() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let config = mcp_config(&runtime);
        assert_eq!(config.allowed_hosts, vec!["resolver.example"]);
        assert_eq!(config.allowed_origins, vec!["https://app.example"]);
        assert!(!config.legacy_session_mode);
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
    async fn exact_origin_middleware_rejects_non_default_port_before_rmcp() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let service = StreamableHttpService::new(
            || Ok::<_, std::io::Error>(TestServer),
            NeverSessionManager::default().into(),
            mcp_config(&runtime),
        );
        let app =
            Router::new()
                .route_service("/mcp", service)
                .layer(middleware::from_fn_with_state(
                    runtime,
                    security::exact_mcp_origin,
                ));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("host", "resolver.example")
                    .header("origin", "https://app.example:4444")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("service response");
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn direct_json_tools_list_is_enriched_end_to_end() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let evidence = Arc::new(AtomicUsize::new(0));
        let service = StreamableHttpService::new(
            {
                let cancelled = cancelled.clone();
                let evidence = evidence.clone();
                move || {
                    Ok::<_, std::io::Error>(TestToolServer::new(
                        cancelled.clone(),
                        evidence.clone(),
                    ))
                }
            },
            NeverSessionManager::default().into(),
            mcp_config(&runtime),
        );
        let app = Router::new()
            .route_service("/mcp", service)
            .layer(middleware::from_fn(mcp::mirror_tools_list_security_schemes));
        let response = app
            .oneshot(mcp_request(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
            ))
            .await
            .expect("tools/list response");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert!(
            response.headers()[axum::http::header::CONTENT_TYPE]
                .to_str()
                .expect("content type")
                .starts_with("application/json")
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
        let tool = &value["result"]["tools"][0];
        assert_eq!(tool["name"], "fetch_source");
        assert_eq!(tool["title"], "Fetch Source");
        assert_eq!(tool["securitySchemes"][0]["type"], "oauth2");
        assert_eq!(tool["_meta"]["securitySchemes"][0]["type"], "oauth2");
        assert!(tool["outputSchema"].is_object());
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert_eq!(tool["annotations"]["destructiveHint"], true);
        assert_eq!(tool["annotations"]["openWorldHint"], true);
    }

    #[tokio::test]
    async fn mcp_timeout_cancels_and_awaits_slow_upstream_work() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let evidence = Arc::new(AtomicUsize::new(0));
        let service = StreamableHttpService::new(
            {
                let cancelled = cancelled.clone();
                let evidence = evidence.clone();
                move || {
                    Ok::<_, std::io::Error>(TestToolServer::new(
                        cancelled.clone(),
                        evidence.clone(),
                    ))
                }
            },
            NeverSessionManager::default().into(),
            mcp_config(&runtime),
        );
        let security = config::SecurityConfig {
            product_body_bytes: config::DEFAULT_PRODUCT_BODY_BYTES,
            product_timeout: Duration::from_secs(1),
            mcp_timeout: Duration::from_millis(20),
            shutdown_grace: Duration::from_secs(1),
            hsts_enabled: false,
        };
        let app =
            Router::new()
                .route_service("/mcp", service)
                .layer(middleware::from_fn_with_state(
                    security,
                    security::mcp_timeout,
                ));
        let response = app
            .oneshot(mcp_request(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fetch_source","arguments":{}}}"#,
            ))
            .await
            .expect("tools/call response");
        assert_eq!(response.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(evidence.load(Ordering::Acquire), 0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(evidence.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn mcp_deadline_bounds_stalled_body_and_drops_downstream() {
        let runtime = config::McpRuntimeConfig {
            allowed_hosts: vec!["resolver.example".into()],
            allowed_origins: vec!["https://app.example".into()],
        };
        let service = StreamableHttpService::new(
            || Ok::<_, std::io::Error>(TestServer),
            NeverSessionManager::default().into(),
            mcp_config(&runtime),
        );
        let security = config::SecurityConfig {
            product_body_bytes: config::DEFAULT_PRODUCT_BODY_BYTES,
            product_timeout: Duration::from_secs(1),
            mcp_timeout: Duration::from_millis(20),
            shutdown_grace: Duration::from_secs(1),
            hsts_enabled: false,
        };
        let app =
            Router::new()
                .route_service("/mcp", service)
                .layer(middleware::from_fn_with_state(
                    security,
                    security::mcp_timeout,
                ));

        let body_dropped = Arc::new(AtomicBool::new(false));
        let drop_flag = DropFlag(body_dropped.clone());
        let stalled_body = futures_util::stream::poll_fn(
            move |_| -> Poll<Option<std::result::Result<String, Infallible>>> {
                let _keep_drop_flag_alive = &drop_flag;
                Poll::Pending
            },
        );
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "resolver.example")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(
                axum::http::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .body(Body::from_stream(stalled_body))
            .expect("stalled MCP request");

        let started = Instant::now();
        let response = app.oneshot(request).await.expect("timeout response");
        assert_eq!(response.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(body_dropped.load(Ordering::Acquire));
    }

    fn mcp_request(body: &'static str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "resolver.example")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(
                axum::http::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .body(Body::from(body))
            .expect("MCP request")
    }

    #[tokio::test]
    async fn graceful_shutdown_drains_readiness_and_cancels_mcp() {
        let state = Arc::new(lifecycle::RuntimeState::new());
        let egress = Arc::new(egress::EgressPolicy::for_tests(&[], true));
        state.mark_ready();
        let cancellation = tokio_util::sync::CancellationToken::new();

        begin_shutdown(&state, &cancellation);

        assert!(cancellation.is_cancelled());
        assert_eq!(
            lifecycle::readiness(Extension(state), Extension(egress))
                .await
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
