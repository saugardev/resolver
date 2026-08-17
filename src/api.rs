//! HTTP handlers that translate product routes into fetcher calls.

use axum::{
    Json,
    extract::{Extension, FromRequest, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::auth::ResolverAuthContext;
use crate::credits::{ResolverCreditAuthorization, ResolverCreditsClient};
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
    headers: HeaderMap,
) -> Result<Json<Receipt>, FetchError> {
    validate_receipt_id(&id)?;
    let logical_request = Fetcher::receipt_billing_context(&id);
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        "receipt",
        None,
        Some(&id),
        &logical_request,
        idempotency_key(&headers)?.as_deref(),
    )
    .await?;
    let receipt = fetcher
        .get_receipt(&id)
        .ok_or_else(|| FetchError::NotFound(format!("Receipt not found: {id}")))?;
    capture_product_route(&credits, &auth_context, "receipt", None, authorization).await?;
    Ok(Json(receipt))
}

pub async fn snapshot_source(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<crate::snapshot_upload::SnapshotPayload>, FetchError> {
    let request = Fetcher::snapshot_request(&payload.source);
    request.validate_for(ProductRoute::Snapshot)?;
    let idempotency_key = idempotency_key(&headers)?;
    let logical_request = fetcher.product_billing_context(&request, ProductRoute::Snapshot)?;
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        "snapshot",
        Some(&payload.source),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch(request, ProductRoute::Snapshot)
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        "snapshot",
        Some(&payload.source),
        authorization,
    )
    .await?;
    let response = fetcher
        .finalize_product_fetch(pending, Some(&auth_context))
        .await?;
    let receipt_id = response
        .receipt_id
        .ok_or_else(|| FetchError::Http("snapshot receipt was not created".into()))?;
    let snapshot = crate::snapshot_upload::SnapshotPayload::from_spider_response(
        &payload.source,
        receipt_id,
        response.data,
        response.provenance,
        response.provenance_error,
    )?;
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

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, FetchError> {
    let mut values = headers.get_all("idempotency-key").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(FetchError::BadRequest(
            "Idempotency-Key must be sent at most once".into(),
        ));
    }
    let value = value
        .to_str()
        .map_err(|_| FetchError::BadRequest("invalid `Idempotency-Key` header".into()))?;
    validate_idempotency_key(Some(value))?;
    Ok(Some(value.trim().to_string()))
}

async fn prepare_and_capture_product(
    fetcher: &Fetcher,
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    payload: ProductRequest,
    route: ProductRoute,
    billing_route: &str,
    requested_idempotency_key: Option<String>,
) -> Result<PendingProductFetch, FetchError> {
    validate_idempotency_key(requested_idempotency_key.as_deref())?;
    payload.validate_for(route)?;
    let logical_request = fetcher.product_billing_context(&payload, route)?;
    let source = payload
        .source
        .as_deref()
        .or(payload.query.as_deref())
        .map(str::to_string);
    let authorization = preflight_product_route(
        credits,
        auth_context,
        billing_route,
        source.as_deref(),
        None,
        &logical_request,
        requested_idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher.prepare_product_fetch(payload, route).await?;
    capture_product_route(
        credits,
        auth_context,
        billing_route,
        source.as_deref(),
        authorization,
    )
    .await?;
    Ok(pending)
}

async fn preflight_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
    source_url: Option<&str>,
    subject_id: Option<&str>,
    logical_request: &Value,
    requested_idempotency_key: Option<&str>,
) -> Result<Option<ResolverCreditAuthorization>, FetchError> {
    match credits
        .preflight_product_request(
            auth_context,
            route,
            source_url,
            subject_id,
            logical_request,
            requested_idempotency_key,
        )
        .await
    {
        Ok(authorization) => Ok(authorization),
        Err(err) if err.is_payment_required() => Err(FetchError::PaymentRequired(err.to_string())),
        Err(err) if err.is_idempotency_conflict() => {
            Err(FetchError::IdempotencyConflict(err.to_string()))
        }
        Err(err) => Err(FetchError::Credits(err.to_string())),
    }
}

async fn capture_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
    source_url: Option<&str>,
    authorization: Option<ResolverCreditAuthorization>,
) -> Result<(), FetchError> {
    match credits.capture_authorized_request(authorization).await {
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
        Err(err) if err.is_idempotency_conflict() => {
            Err(FetchError::IdempotencyConflict(err.to_string()))
        }
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
        capture_behavior: CaptureBehavior,
        ledger_entries: Arc<Mutex<Vec<Value>>>,
    }

    #[derive(Clone, Copy)]
    enum CaptureBehavior {
        Success,
        Uncharged,
        Unenforced,
        AmountMismatch,
        Replay,
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
        let idempotency_key = payload["idempotency_key"]
            .as_str()
            .expect("backend idempotency key");
        assert!(idempotency_key.starts_with("resolver_"));
        assert_eq!(idempotency_key.rsplit_once(':').unwrap().1.len(), 64);
        let metadata = payload["metadata"].clone();
        assert_eq!(metadata["request_fingerprint"].as_str().unwrap().len(), 64);
        assert_eq!(metadata["pricing_version"], "resolver-pricing-v1");
        let mut ledger_entry = json!({
            "tenant_id": "tenant-a",
            "project_id": payload["project_id"],
            "entry_type": "debit",
            "amount_delta": -payload["amount"].as_i64().expect("capture amount"),
            "idempotency_key": idempotency_key,
            "metadata": metadata,
        });

        match state.capture_behavior {
            CaptureBehavior::Uncharged => {
                state
                    .events
                    .lock()
                    .expect("events")
                    .push("credits_rejected");
                Json(json!({
                    "mode": "enforce",
                    "enforced": true,
                    "charged": false,
                    "amount": payload["amount"],
                    "reason": "capture was not applied",
                    "ledger_entry": ledger_entry,
                }))
                .into_response()
            }
            CaptureBehavior::Unenforced => {
                state
                    .events
                    .lock()
                    .expect("events")
                    .push("credits_rejected");
                Json(json!({
                    "mode": "shadow",
                    "enforced": false,
                    "charged": true,
                    "amount": payload["amount"],
                    "ledger_entry": ledger_entry,
                }))
                .into_response()
            }
            CaptureBehavior::AmountMismatch => {
                state
                    .events
                    .lock()
                    .expect("events")
                    .push("credits_rejected");
                Json(json!({
                    "mode": "captured",
                    "enforced": true,
                    "charged": true,
                    "amount": payload["amount"].as_i64().expect("capture amount") + 1,
                    "ledger_entry": ledger_entry,
                }))
                .into_response()
            }
            CaptureBehavior::Replay => {
                ledger_entry["metadata"]["capture_attempt_id"] = json!("prior-attempt");
                state.events.lock().expect("events").push("credits_replay");
                Json(json!({
                    "mode": "idempotent_replay",
                    "enforced": true,
                    "charged": true,
                    "amount": payload["amount"],
                    "ledger_entry": ledger_entry,
                }))
                .into_response()
            }
            CaptureBehavior::Success => {
                state.captures.fetch_add(1, Ordering::SeqCst);
                state.events.lock().expect("events").push("credits");
                state
                    .ledger_entries
                    .lock()
                    .expect("ledger entries")
                    .push(ledger_entry.clone());
                Json(json!({
                    "mode": "captured",
                    "enforced": true,
                    "charged": true,
                    "amount": payload["amount"],
                    "ledger_entry": ledger_entry,
                }))
                .into_response()
            }
        }
    }

    async fn ledger(State(state): State<BoundaryState>) -> Json<Value> {
        state.events.lock().expect("events").push("ledger");
        Json(Value::Array(
            state.ledger_entries.lock().expect("ledger entries").clone(),
        ))
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
        boundary_app_with_capture(
            spider,
            max_response_bytes,
            events,
            balance,
            CaptureBehavior::Success,
        )
        .await
    }

    async fn boundary_app_with_capture(
        spider: Router,
        max_response_bytes: usize,
        events: Arc<Mutex<Vec<&'static str>>>,
        balance: i64,
        capture_behavior: CaptureBehavior,
    ) -> (
        Router,
        Arc<Fetcher>,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let captures = Arc::new(AtomicUsize::new(0));
        let provenance_calls = Arc::new(AtomicUsize::new(0));
        let ledger_entries = Arc::new(Mutex::new(Vec::new()));
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
            capture_behavior,
            ledger_entries,
        };
        let control = Router::new()
            .route("/oauth/introspect", post(introspect))
            .route("/api/v1/tenants/tenant-a/users/me/credits", get(preflight))
            .route(
                "/api/v1/tenants/tenant-a/users/me/credit-ledger",
                get(ledger),
            )
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

    fn product_request_with_key(path: &str, body: Value, key: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))
            .expect("fetch request")
    }

    fn product_request(path: &str, body: Value) -> Request<Body> {
        product_request_with_key(path, body, "request-1")
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
            vec![
                "oauth",
                "ledger",
                "preflight",
                "spider",
                "credits",
                "provenance"
            ]
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
            vec!["oauth", "ledger", "preflight", "spider"]
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
        assert_eq!(
            *events.lock().expect("events"),
            vec!["oauth", "ledger", "preflight"]
        );
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
            boundary_app_with_capture(spider, 4096, events, 10, CaptureBehavior::Uncharged).await;

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
            vec!["oauth", "ledger", "preflight", "spider", "credits_rejected"]
        );
    }

    #[tokio::test]
    async fn shadow_and_amount_mismatch_capture_outcomes_never_finalize() {
        for behavior in [CaptureBehavior::Unenforced, CaptureBehavior::AmountMismatch] {
            let spider = Router::new().route(
                "/scrape",
                post(|| async { Json(json!([{"status": 200, "content": "resolved"}])) }),
            );
            let events = Arc::new(Mutex::new(Vec::new()));
            let (app, fetcher, _, captures, provenance_calls) =
                boundary_app_with_capture(spider, 4096, events, 10, behavior).await;

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
        }
    }

    #[tokio::test]
    async fn same_caller_key_replay_and_conflict_do_not_duplicate_work_or_evidence() {
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
        let (app, fetcher, _, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;
        let first_body = json!({
            "source": "https://example.com/a",
            "mode": "raw",
            "receipt": true
        });

        let first = app
            .clone()
            .oneshot(product_request_with_key(
                "/fetch",
                first_body.clone(),
                "stable-request",
            ))
            .await
            .expect("first response");
        assert_eq!(first.status(), StatusCode::OK);

        let replay = app
            .clone()
            .oneshot(product_request_with_key(
                "/fetch",
                first_body,
                "stable-request",
            ))
            .await
            .expect("replay response");
        assert_eq!(replay.status(), StatusCode::CONFLICT);

        let conflict = app
            .oneshot(product_request_with_key(
                "/fetch",
                json!({
                    "source": "https://example.com/b",
                    "mode": "raw",
                    "receipt": true
                }),
                "stable-request",
            ))
            .await
            .expect("conflict response");
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(spider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(captures.load(Ordering::SeqCst), 1);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fetcher.receipt_count(), 1);
    }

    #[tokio::test]
    async fn unbound_backend_replay_fails_closed_without_new_evidence() {
        let spider = Router::new().route(
            "/scrape",
            post(|| async { Json(json!([{"status": 200, "content": "resolved"}])) }),
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let (app, fetcher, _, captures, provenance_calls) =
            boundary_app_with_capture(spider, 4096, events, 10, CaptureBehavior::Replay).await;

        let response = app
            .oneshot(fetch_request(json!({
                "source": "https://example.com",
                "mode": "raw",
                "receipt": true
            })))
            .await
            .expect("resolver response");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.receipt_count(), 0);
    }

    #[tokio::test]
    async fn snapshot_captures_then_finalizes_one_resolver_receipt_and_provenance() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spider_events = events.clone();
        let spider = Router::new().route(
            "/scrape",
            post(move || {
                let events = spider_events.clone();
                async move {
                    events.lock().expect("events").push("spider");
                    Json(json!([{
                        "id": "upstream-receipt-must-not-be-used",
                        "status": 200,
                        "raw": "<html>snapshot</html>",
                        "screenshot": "c25hcHNob3Q="
                    }]))
                }
            }),
        );
        let (app, fetcher, events, captures, provenance_calls) =
            boundary_app(spider, 4096, events, 10).await;

        let response = app
            .oneshot(product_request_with_key(
                "/snapshot",
                json!({"source": "https://example.com"}),
                "snapshot-1",
            ))
            .await
            .expect("snapshot response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("snapshot body");
        let body: Value = serde_json::from_slice(&body).expect("snapshot JSON");
        assert_eq!(body["html"], "<html>snapshot</html>");
        assert_eq!(body["screenshot_base64"], "c25hcHNob3Q=");
        assert_ne!(body["receipt_id"], "upstream-receipt-must-not-be-used");
        assert_eq!(captures.load(Ordering::SeqCst), 1);
        assert_eq!(provenance_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fetcher.receipt_count(), 1);
        assert_eq!(
            *events.lock().expect("events"),
            vec![
                "oauth",
                "ledger",
                "preflight",
                "spider",
                "credits",
                "provenance"
            ]
        );
    }

    #[tokio::test]
    async fn adaptive_modes_never_follow_or_fallback_control_plane_redirects() {
        for mode in ["auto", "fast"] {
            for status in [
                StatusCode::MOVED_PERMANENTLY,
                StatusCode::FOUND,
                StatusCode::TEMPORARY_REDIRECT,
                StatusCode::PERMANENT_REDIRECT,
            ] {
                let unblock_calls = Arc::new(AtomicUsize::new(0));
                let redirect_target_calls = Arc::new(AtomicUsize::new(0));
                let unblock_counter = unblock_calls.clone();
                let target_counter = redirect_target_calls.clone();
                let spider = Router::new()
                    .route(
                        "/scrape",
                        post(move || async move {
                            axum::http::Response::builder()
                                .status(status)
                                .header(header::LOCATION, "/redirect-target")
                                .header(header::CONTENT_TYPE, "text/plain")
                                .body(Body::from("captcha verification required"))
                                .expect("redirect response")
                        }),
                    )
                    .route(
                        "/unblocker",
                        post(move || {
                            let calls = unblock_counter.clone();
                            async move {
                                calls.fetch_add(1, Ordering::SeqCst);
                                Json(json!([{"status": 200, "content": "unblocked"}]))
                            }
                        }),
                    )
                    .route(
                        "/redirect-target",
                        post(move || {
                            let calls = target_counter.clone();
                            async move {
                                calls.fetch_add(1, Ordering::SeqCst);
                                Json(json!([{"status": 200, "content": "redirected"}]))
                            }
                        }),
                    );
                let events = Arc::new(Mutex::new(Vec::new()));
                let (app, fetcher, _, captures, provenance_calls) =
                    boundary_app(spider, 4096, events, 10).await;

                let response = app
                    .oneshot(product_request_with_key(
                        "/fetch",
                        json!({
                            "source": "https://example.com",
                            "mode": mode,
                            "receipt": true
                        }),
                        &format!("redirect-{mode}-{}", status.as_u16()),
                    ))
                    .await
                    .expect("redirect response");

                assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
                assert_eq!(unblock_calls.load(Ordering::SeqCst), 0);
                assert_eq!(redirect_target_calls.load(Ordering::SeqCst), 0);
                assert_eq!(captures.load(Ordering::SeqCst), 0);
                assert_eq!(provenance_calls.load(Ordering::SeqCst), 0);
                assert_eq!(fetcher.receipt_count(), 0);
            }
        }
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

        for (index, (path, body)) in cases.into_iter().enumerate() {
            let response = app
                .clone()
                .oneshot(product_request_with_key(
                    path,
                    body,
                    &format!("request-{index}"),
                ))
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
