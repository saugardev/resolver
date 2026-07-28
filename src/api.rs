//! HTTP handlers that translate product routes into fetcher calls.

use axum::{
    Json,
    extract::{Extension, FromRequest, Path, Request, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::auth::ResolverAuthContext;
use crate::credits::ResolverCreditsClient;
use crate::errors::FetchError;
use crate::fetch::Fetcher;
use crate::types::{
    FetchWithReceipt, ProductRequest, ProductResponse, ProductRoute, ProvenanceOptions, Receipt,
    validate_receipt_id, validate_source_url,
};
use serde::Deserialize;
use serde_json::Value;
use std::{sync::Arc, time::Instant};

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
    pub provenance: Option<bool>,
    pub publish_to_arweave: Option<bool>,
    pub register_onchain: Option<bool>,
    pub wait_for_publication: Option<bool>,
}

impl FetchRequest {
    fn provenance_options(&self) -> ProvenanceOptions {
        ProvenanceOptions {
            provenance: self.provenance == Some(true),
            publish_to_arweave: self.publish_to_arweave == Some(true),
            register_onchain: self.register_onchain == Some(true),
            wait_for_publication: self.wait_for_publication == Some(true),
        }
        .normalized()
    }
}

pub async fn fetch_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Scrape)?;
    let started = Instant::now();
    let source = payload.source.clone();
    let options = payload.provenance_options();
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Scrape, Some(&auth_context))
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "fetch",
        source.as_deref(),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

pub async fn crawl_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Crawl)?;
    let started = Instant::now();
    let source = payload.source.clone();
    let options = payload.provenance_options();
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Crawl, Some(&auth_context))
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "crawl",
        source.as_deref(),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

pub async fn map_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Map)?;
    let started = Instant::now();
    let source = payload.source.clone();
    let options = payload.provenance_options();
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Map, Some(&auth_context))
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "map",
        source.as_deref(),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

pub async fn search_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Search)?;
    let started = Instant::now();
    let options = payload.provenance_options();
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Search, Some(&auth_context))
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "search",
        None,
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

pub async fn extract_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Extract)?;
    let started = Instant::now();
    let source = payload.source.clone();
    let options = payload.provenance_options();
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Extract, Some(&auth_context))
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "extract",
        source.as_deref(),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

pub async fn screenshot_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Screenshot)?;
    let started = Instant::now();
    let source = payload.source.clone();
    let options = payload.provenance_options();
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Screenshot, Some(&auth_context))
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "screenshot",
        source.as_deref(),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

pub async fn fetch_fast(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<FetchWithReceipt>, FetchError> {
    validate_source_url(&payload.source)?;
    let started = Instant::now();
    let options = payload.provenance_options();
    options.validate()?;
    let data = fetcher
        .get_fast_data_with_receipt_with_options(&payload.source, Some(&auth_context), options)
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "fetchfast",
        Some(&payload.source),
        None,
        started,
        options,
    )
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
    let started = Instant::now();
    let receipt = fetcher
        .get_receipt(&id)
        .ok_or_else(|| FetchError::NotFound(format!("Receipt not found: {id}")))?;
    debit_product_route(
        &credits,
        &auth_context,
        "receipt",
        None,
        Some(&id),
        started,
        ProvenanceOptions::default(),
    )
    .await?;
    Ok(Json(receipt))
}

pub async fn snapshot_source(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<crate::snapshot_upload::SnapshotPayload>, FetchError> {
    validate_source_url(&payload.source)?;
    let started = Instant::now();
    let options = payload.provenance_options();
    options.validate()?;
    let snapshot = fetcher
        .snapshot_with_receipt_with_options(&payload.source, Some(&auth_context), options)
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "snapshot",
        Some(&payload.source),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(snapshot))
}

#[axum::debug_handler]
pub async fn fetch_unblock(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<Value>, FetchError> {
    validate_source_url(&payload.source)?;
    let started = Instant::now();
    let options = payload.provenance_options();
    options.validate()?;
    let data = fetcher
        .unblocker_with_options(&payload.source, Some(&auth_context), options)
        .await?;
    debit_product_route(
        &credits,
        &auth_context,
        "fetchunblock",
        Some(&payload.source),
        None,
        started,
        options,
    )
    .await?;
    Ok(Json(data))
}

async fn debit_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
    source_url: Option<&str>,
    subject_id: Option<&str>,
    started: Instant,
    options: ProvenanceOptions,
) -> Result<(), FetchError> {
    let charge = credits.charge_for(route, started.elapsed(), options);
    match credits
        .debit_product_request(auth_context, route, source_url, subject_id, charge, options)
        .await
    {
        Ok(Some(outcome)) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "resolver_credit_debit",
                    "request_id": crate::security::current_request_id(),
                    "route": route,
                    "tenant_id": auth_context.tenant_id.as_deref(),
                    "project_id": auth_context.project_id.as_deref(),
                    "source_sha256": source_url.map(crate::security::sensitive_hash),
                    "charged": outcome.charged,
                    "amount": outcome.amount,
                    "elapsed_ms": charge.elapsed_ms,
                    "credit_budget": charge.budget,
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
                    "event": "resolver_credit_debit_skipped",
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
