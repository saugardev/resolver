//! HTTP handlers that translate product routes into fetcher calls.

use axum::{
    Json,
    extract::{Extension, FromRequest, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::auth::ResolverAuthContext;
use crate::credits::ResolverCreditsClient;
use crate::errors::FetchError;
use crate::fetch::{Fetcher, PendingProductFetch};
use crate::types::{
    FetchWithReceipt, ProductRequest, ProductResponse, ProductRoute, Receipt,
    validate_idempotency_key, validate_receipt_id, validate_source_url,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection_response(rejection)),
        }
    }
}

fn json_rejection_response(rejection: JsonRejection) -> Response {
    let status = rejection.status();
    let (status, code, message) = if status == StatusCode::PAYLOAD_TOO_LARGE {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Request body is too large",
        )
    } else if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be application/json",
        )
    } else {
        (StatusCode::BAD_REQUEST, "invalid_json", "Invalid JSON body")
    };
    (
        status,
        Json(serde_json::json!({
            "error": message,
            "code": code,
            "request_id": crate::security::current_request_id(),
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct FetchRequest {
    pub source: String,
}

pub async fn fetch_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        payload,
        ProductRoute::Scrape,
        ProductRoute::Scrape.as_str(),
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn crawl_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        payload,
        ProductRoute::Crawl,
        ProductRoute::Crawl.as_str(),
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn map_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        payload,
        ProductRoute::Map,
        ProductRoute::Map.as_str(),
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn search_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        payload,
        ProductRoute::Search,
        ProductRoute::Search.as_str(),
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn extract_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        payload,
        ProductRoute::Extract,
        ProductRoute::Extract.as_str(),
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn screenshot_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        payload,
        ProductRoute::Screenshot,
        ProductRoute::Screenshot.as_str(),
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn fetch_fast(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<FetchWithReceipt>, FetchError> {
    validate_source_url(&payload.source)?;
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        Fetcher::fast_request(&payload.source),
        ProductRoute::Scrape,
        "fetchfast",
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_fetch_with_receipt(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn get_receipt(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Path(id): Path<String>,
) -> Result<Json<Receipt>, FetchError> {
    validate_receipt_id(&id)?;
    let receipt = fetcher
        .get_receipt(&id)
        .ok_or_else(|| FetchError::NotFound(format!("Receipt not found: {id}")))?;
    preflight_product_route(&credits, &auth_context, "receipt").await?;
    capture_product_route(&credits, &auth_context, "receipt", None, Some(&id), None).await?;
    Ok(Json(receipt))
}

pub async fn snapshot_source(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<crate::snapshot_upload::SnapshotPayload>, FetchError> {
    validate_source_url(&payload.source)?;
    let idempotency_key = idempotency_key(&headers)?;
    preflight_product_route(&credits, &auth_context, "snapshot").await?;
    let snapshot = fetcher.prepare_snapshot(&payload.source).await?;
    capture_product_route(
        &credits,
        &auth_context,
        "snapshot",
        Some(&payload.source),
        None,
        idempotency_key,
    )
    .await?;
    Ok(Json(snapshot))
}

#[axum::debug_handler]
pub async fn fetch_unblock(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<Value>, FetchError> {
    validate_source_url(&payload.source)?;
    let pending = prepare_and_capture_product(
        &fetcher,
        &credits,
        &auth_context,
        Fetcher::unblock_request(&payload.source, false),
        ProductRoute::Unblock,
        "fetchunblock",
        idempotency_key(&headers)?,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    Ok(Json(data.data))
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<&str>, FetchError> {
    let value = headers
        .get("idempotency-key")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| FetchError::BadRequest("invalid `Idempotency-Key` header".into()))
        })
        .transpose()?;
    validate_idempotency_key(value)?;
    Ok(value)
}

async fn prepare_and_capture_product(
    fetcher: &Fetcher,
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    payload: ProductRequest,
    route: ProductRoute,
    billing_route: &str,
    requested_idempotency_key: Option<&str>,
) -> Result<PendingProductFetch, FetchError> {
    validate_idempotency_key(requested_idempotency_key)?;
    payload.validate_for(route)?;
    preflight_product_route(credits, auth_context, billing_route).await?;
    let pending = fetcher.prepare_product_fetch(payload, route).await?;
    let source = pending.source_or_query().map(str::to_string);
    capture_product_route(
        credits,
        auth_context,
        billing_route,
        source.as_deref(),
        None,
        requested_idempotency_key,
    )
    .await?;
    Ok(pending)
}

async fn preflight_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
) -> Result<(), FetchError> {
    match credits.preflight(auth_context).await {
        Ok(()) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "resolver_credit_preflight_ok",
                    "request_id": crate::security::current_request_id(),
                    "route": route,
                    "tenant_id": auth_context.tenant_id.as_deref(),
                    "project_id": auth_context.project_id.as_deref(),
                })
            );
            Ok(())
        }
        Err(err) if err.is_payment_required() => Err(FetchError::PaymentRequired(err.to_string())),
        Err(err) => Err(FetchError::Credits(err.to_string())),
    }
}

async fn capture_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
    source_url: Option<&str>,
    subject_id: Option<&str>,
    requested_idempotency_key: Option<&str>,
) -> Result<(), FetchError> {
    match credits
        .capture_product_request(
            auth_context,
            route,
            source_url,
            subject_id,
            requested_idempotency_key,
        )
        .await
    {
        Ok(Some(outcome)) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "resolver_credit_capture",
                    "request_id": crate::security::current_request_id(),
                    "route": route,
                    "tenant_id": auth_context.tenant_id.as_deref(),
                    "project_id": auth_context.project_id.as_deref(),
                    "source_sha256": source_url.map(crate::security::sensitive_hash),
                    "charged": outcome.charged,
                    "amount": outcome.amount,
                    "mode": outcome.mode,
                    "enforced": outcome.enforced,
                })
            );
            Ok(())
        }
        Ok(None) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "resolver_credit_capture_skipped",
                    "request_id": crate::security::current_request_id(),
                    "route": route,
                    "tenant_id": auth_context.tenant_id.as_deref(),
                    "project_id": auth_context.project_id.as_deref(),
                    "source_sha256": source_url.map(crate::security::sensitive_hash),
                })
            );
            Ok(())
        }
        Err(err) if err.is_payment_required() => Err(FetchError::PaymentRequired(err.to_string())),
        Err(err) => Err(FetchError::Credits(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::ResolverAuth,
        errors::ProvenanceError,
        fetch::{ProvenanceAttestor, ProvenanceFuture},
        provenance::ResolverFetchEvidence,
    };
    use axum::{
        Router,
        body::{Body, Bytes, to_bytes},
        extract::State,
        http::{Request, StatusCode, Uri, header},
        middleware,
        routing::{get, post},
    };
    use serde_json::{Value, json};
    use std::{
        convert::Infallible,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    use tokio_stream::wrappers::ReceiverStream;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct BoundaryState {
        events: Arc<Mutex<Vec<&'static str>>>,
        captures: Arc<AtomicUsize>,
        provenance_calls: Arc<AtomicUsize>,
        fetcher: Arc<Fetcher>,
        balance: i64,
        capture_succeeds: bool,
    }

    struct RecordingProvenance {
        events: Arc<Mutex<Vec<&'static str>>>,
        calls: Arc<AtomicUsize>,
    }

    impl ProvenanceAttestor for RecordingProvenance {
        fn attest_fetch<'a>(
            &'a self,
            evidence: ResolverFetchEvidence<'a>,
            auth_context: Option<&'a ResolverAuthContext>,
        ) -> ProvenanceFuture<'a> {
            assert!(
                evidence.receipt.is_some(),
                "receipt must precede provenance"
            );
            assert_eq!(
                auth_context.and_then(|context| context.tenant_id.as_deref()),
                Some("tenant-a")
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.events.lock().expect("events").push("provenance");
            Box::pin(async {
                Err(ProvenanceError::Attestation(
                    "intentional test failure".to_string(),
                ))
            })
        }
    }

    async fn spawn(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind boundary mock");
        let address = listener.local_addr().expect("boundary address");
        tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve boundary mock");
        });
        format!("http://{address}")
    }

    async fn introspect(State(state): State<BoundaryState>) -> Json<Value> {
        state.events.lock().expect("events").push("oauth");
        Json(json!({
            "active": true,
            "scope": "resolver:source:fetch resolver:source:crawl resolver:source:map resolver:source:search resolver:source:extract resolver:source:screenshot resolver:snapshot:create resolver:receipt:read",
            "aud": "https://resolver.api.livylabs.xyz/mcp",
            "client_id": "test-client",
            "https://claims.livylabs.xyz/tenant_id": "tenant-a",
            "https://claims.livylabs.xyz/project_id": "project-a"
        }))
    }

    async fn capture(State(state): State<BoundaryState>, Json(payload): Json<Value>) -> Response {
        assert_eq!(state.fetcher.receipt_count(), 0);
        assert_eq!(state.provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(payload["idempotency_key"], "resolver_fetch:request-1");
        if !state.capture_succeeds {
            state
                .events
                .lock()
                .expect("events")
                .push("credits_rejected");
            return Json(json!({
                "mode": "enforce",
                "enforced": true,
                "charged": false,
                "amount": 1,
                "reason": "capture was not applied"
            }))
            .into_response();
        }
        state.captures.fetch_add(1, Ordering::SeqCst);
        state.events.lock().expect("events").push("credits");
        Json(json!({
            "mode": "captured",
            "enforced": true,
            "charged": true,
            "amount": 1
        }))
        .into_response()
    }

    async fn preflight(State(state): State<BoundaryState>) -> Json<Value> {
        state.events.lock().expect("events").push("preflight");
        Json(json!({
            "tenant_id": "tenant-a",
            "membership_id": "00000000-0000-0000-0000-000000000001",
            "balance": state.balance,
            "lifetime_granted": 10,
            "lifetime_used": 0,
            "created_at": "2026-08-17T00:00:00Z",
            "updated_at": "2026-08-17T00:00:00Z"
        }))
    }

    async fn boundary_app(
        spider: Router,
        max_response_bytes: usize,
        events: Arc<Mutex<Vec<&'static str>>>,
        balance: i64,
    ) -> (
        Router,
        Arc<Fetcher>,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        boundary_app_with_capture(spider, max_response_bytes, events, balance, true).await
    }

    async fn boundary_app_with_capture(
        spider: Router,
        max_response_bytes: usize,
        events: Arc<Mutex<Vec<&'static str>>>,
        balance: i64,
        capture_succeeds: bool,
    ) -> (
        Router,
        Arc<Fetcher>,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let captures = Arc::new(AtomicUsize::new(0));
        let provenance_calls = Arc::new(AtomicUsize::new(0));
        let spider_base = spawn(spider).await;
        let provenance = Arc::new(RecordingProvenance {
            events: events.clone(),
            calls: provenance_calls.clone(),
        });
        let fetcher = Arc::new(Fetcher::for_tests(
            spider_base,
            max_response_bytes,
            Some(provenance),
        ));
        let state = BoundaryState {
            events: events.clone(),
            captures: captures.clone(),
            provenance_calls: provenance_calls.clone(),
            fetcher: fetcher.clone(),
            balance,
            capture_succeeds,
        };
        let control = Router::new()
            .route("/oauth/introspect", post(introspect))
            .route("/api/v1/tenants/tenant-a/users/me/credits", get(preflight))
            .route(
                "/api/v1/tenants/tenant-a/users/me/credits/debits",
                post(capture),
            )
            .with_state(state);
        let control_base = spawn(control).await;
        let auth = Arc::new(ResolverAuth::for_test_endpoint(format!(
            "{control_base}/oauth/introspect"
        )));
        let credits = Arc::new(ResolverCreditsClient::for_tests(control_base));
        let app = Router::new()
            .route("/fetch", post(fetch_post))
            .route("/crawl", post(crawl_post))
            .route("/map", post(map_post))
            .route("/search", post(search_post))
            .route("/extract", post(extract_post))
            .route("/screenshot", post(screenshot_post))
            .route("/snapshot", post(snapshot_source))
            .route("/fetchfast", post(fetch_fast))
            .route("/fetchunblock", post(fetch_unblock))
            .layer(Extension(credits))
            .route_layer(middleware::from_fn_with_state(
                auth,
                crate::auth::require_product_oauth,
            ))
            .with_state(fetcher.clone());

        (app, fetcher, events, captures, provenance_calls)
    }

    fn product_request(path: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "request-1")
            .body(Body::from(body.to_string()))
            .expect("fetch request")
    }

    fn fetch_request(body: Value) -> Request<Body> {
        product_request("/fetch", body)
    }

    #[tokio::test]
    async fn oauth_spider_capture_receipt_and_provenance_are_strictly_ordered() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spider_events = events.clone();
        let spider = Router::new().route(
            "/scrape",
            post(move || {
                let events = spider_events.clone();
                async move {
                    events.lock().expect("events").push("spider");
                    Json(json!([{"status": 200, "content": "resolved"}]))
                }
            }),
        );
        let (app, fetcher, boundary_events, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("resolver body");
        let response: Value = serde_json::from_slice(&body).expect("response JSON");
        assert!(response["receipt_id"].is_string());
        assert_eq!(captures.load(Ordering::SeqCst), 1);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fetcher.receipt_count(), 1);
        assert_eq!(
            *boundary_events.lock().expect("boundary events"),
            vec!["oauth", "preflight", "spider", "credits", "provenance"]
        );
    }

    #[tokio::test]
    async fn upstream_failure_never_captures_or_finalizes() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spider_events = events.clone();
        let spider = Router::new().route(
            "/scrape",
            post(move || {
                let events = spider_events.clone();
                async move {
                    events.lock().expect("events").push("spider");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"message": "upstream unavailable"})),
                    )
                }
            }),
        );
        let (app, fetcher, events, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
        assert_eq!(
            *events.lock().expect("events"),
            vec!["oauth", "preflight", "spider"]
        );
    }

    #[tokio::test]
    async fn insufficient_balance_prevents_spider_capture_and_finalization() {
        let spider_calls = Arc::new(AtomicUsize::new(0));
        let calls = spider_calls.clone();
        let spider = Router::new().route(
            "/scrape",
            post(move || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Json(json!([{"status": 200, "content": "resolved"}]))
                }
            }),
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let (app, fetcher, events, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 0).await;

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("payment body");
        let body: Value = serde_json::from_slice(&body).expect("payment JSON");
        assert_eq!(body["code"], "payment_required");
        assert_eq!(spider_calls.load(Ordering::SeqCst), 0);
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
        assert_eq!(*events.lock().expect("events"), vec!["oauth", "preflight"]);
    }

    #[tokio::test]
    async fn authoritative_capture_rejection_discards_pending_result_without_evidence() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spider_events = events.clone();
        let spider = Router::new().route(
            "/scrape",
            post(move || {
                let events = spider_events.clone();
                async move {
                    events.lock().expect("events").push("spider");
                    Json(json!([{"status": 200, "content": "resolved"}]))
                }
            }),
        );
        let (app, fetcher, events, captures, provenance_calls) =
            boundary_app_with_capture(spider, 4096, events, 10, false).await;

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
        assert_eq!(
            *events.lock().expect("events"),
            vec!["oauth", "preflight", "spider", "credits_rejected"]
        );
    }

    #[tokio::test]
    async fn every_http_fetch_path_fails_before_capture_and_hits_the_exact_spider_endpoint() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spider_paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_paths = spider_paths.clone();
        let spider = Router::new().fallback(move |uri: Uri| {
            let paths = recorded_paths.clone();
            async move {
                paths.lock().expect("paths").push(uri.path().to_string());
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": "upstream unavailable"})),
                )
            }
        });
        let (app, fetcher, events, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;

        let cases = [
            (
                "/fetch",
                json!({"source": "https://example.com", "mode": "raw", "receipt": true}),
            ),
            ("/crawl", json!({"source": "https://example.com"})),
            ("/map", json!({"source": "https://example.com"})),
            ("/search", json!({"query": "resolver"})),
            ("/extract", json!({"source": "https://example.com"})),
            ("/screenshot", json!({"source": "https://example.com"})),
            ("/snapshot", json!({"source": "https://example.com"})),
            ("/fetchfast", json!({"source": "https://example.com"})),
            ("/fetchunblock", json!({"source": "https://example.com"})),
        ];

        for (path, body) in cases {
            let response = app
                .clone()
                .oneshot(product_request(path, body))
                .await
                .expect("resolver response");
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "{path}");
        }

        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
        assert_eq!(
            events
                .lock()
                .expect("events")
                .iter()
                .filter(|event| **event == "preflight")
                .count(),
            9
        );
        assert_eq!(
            *spider_paths.lock().expect("paths"),
            vec![
                "/scrape",
                "/crawl",
                "/links",
                "/search",
                "/scrape",
                "/screenshot",
                "/scrape",
                "/scrape",
                "/unblocker",
            ]
        );
    }

    #[tokio::test]
    async fn chunked_oversize_without_content_length_never_captures_or_finalizes() {
        let spider = Router::new().route(
            "/scrape",
            post(|| async {
                let chunks = tokio_stream::iter([
                    Ok::<Bytes, Infallible>(Bytes::from("x".repeat(40))),
                    Ok::<Bytes, Infallible>(Bytes::from("y".repeat(40))),
                ]);
                axum::http::Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from_stream(chunks))
                    .expect("chunked response")
            }),
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let (app, fetcher, _, captures, provenance_calls) =
            boundary_app(spider, 64, events, 10).await;

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("error body");
        let body: Value = serde_json::from_slice(&body).expect("error JSON");
        assert_eq!(body["code"], "upstream_response_too_large");
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
    }

    #[tokio::test]
    async fn stalled_response_body_times_out_before_capture_or_finalize() {
        let spider = Router::new().route(
            "/scrape",
            post(|| async {
                let (sender, receiver) = tokio::sync::mpsc::channel(2);
                sender
                    .send(Ok::<Bytes, Infallible>(Bytes::from_static(
                        br#"[{"status":200,"content":"#,
                    )))
                    .await
                    .expect("first chunk");
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = sender.send(Ok(Bytes::from_static(br#"resolved"}]"#))).await;
                });
                axum::http::Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from_stream(ReceiverStream::new(receiver)))
                    .expect("stalled response")
            }),
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let (app, fetcher, _, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;
        let started = Instant::now();

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "timeout_secs": 1,
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert!(started.elapsed() < Duration::from_millis(1_400));
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
    }

    #[tokio::test]
    async fn adaptive_attempts_share_one_deadline_and_never_capture_on_timeout() {
        let scrape_calls = Arc::new(AtomicUsize::new(0));
        let unblock_calls = Arc::new(AtomicUsize::new(0));
        let scrape_counter = scrape_calls.clone();
        let unblock_counter = unblock_calls.clone();
        let spider = Router::new()
            .route(
                "/scrape",
                post(move || {
                    let calls = scrape_counter.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(700)).await;
                        Json(json!([{
                            "status": 200,
                            "content": "captcha verification required"
                        }]))
                    }
                }),
            )
            .route(
                "/unblocker",
                post(move || {
                    let calls = unblock_counter.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(700)).await;
                        Json(json!([{"status": 200, "content": "resolved"}]))
                    }
                }),
            );
        let events = Arc::new(Mutex::new(Vec::new()));
        let (app, fetcher, _, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;
        let started = Instant::now();

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "fast",
                "timeout_secs": 1,
                "receipt": true
            })))
            .await
            .expect("resolver response");
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert!(started.elapsed() < Duration::from_millis(1_400));
        assert_eq!(scrape_calls.load(Ordering::SeqCst), 1);
        assert_eq!(unblock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
    }
}
