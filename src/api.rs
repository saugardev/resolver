//! HTTP handlers that translate product routes into fetcher calls.

use axum::{
    Json,
    extract::{Extension, FromRequest, Path, Request, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::auth::ResolverAuthContext;
use crate::credits::ResolverCreditsClient;
use crate::egress::EgressPolicy;
use crate::errors::FetchError;
use crate::fetch::Fetcher;
use crate::types::{
    FetchWithReceipt, ProductRequest, ProductResponse, ProductRoute, Receipt, validate_receipt_id,
    validate_source_url,
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
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Scrape)?;
    egress.validate_source(payload.require_source()?).await?;
    debit_product_route(
        &credits,
        &auth_context,
        ProductRoute::Scrape.as_str(),
        payload.source.as_deref(),
        None,
    )
    .await?;
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Scrape, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn crawl_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Crawl)?;
    egress.validate_source(payload.require_source()?).await?;
    debit_product_route(
        &credits,
        &auth_context,
        ProductRoute::Crawl.as_str(),
        payload.source.as_deref(),
        None,
    )
    .await?;
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Crawl, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn map_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Map)?;
    egress.validate_source(payload.require_source()?).await?;
    debit_product_route(
        &credits,
        &auth_context,
        ProductRoute::Map.as_str(),
        payload.source.as_deref(),
        None,
    )
    .await?;
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Map, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn search_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Search)?;
    egress.require_actual_fetch_capability().await?;
    debit_product_route(
        &credits,
        &auth_context,
        ProductRoute::Search.as_str(),
        None,
        None,
    )
    .await?;
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Search, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn extract_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Extract)?;
    egress.validate_source(payload.require_source()?).await?;
    debit_product_route(
        &credits,
        &auth_context,
        ProductRoute::Extract.as_str(),
        payload.source.as_deref(),
        None,
    )
    .await?;
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Extract, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn screenshot_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Screenshot)?;
    egress.validate_source(payload.require_source()?).await?;
    debit_product_route(
        &credits,
        &auth_context,
        ProductRoute::Screenshot.as_str(),
        payload.source.as_deref(),
        None,
    )
    .await?;
    let data = fetcher
        .product_fetch_with_auth(payload, ProductRoute::Screenshot, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

pub async fn fetch_fast(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<FetchWithReceipt>, FetchError> {
    validate_source_url(&payload.source)?;
    egress.validate_source(&payload.source).await?;
    debit_product_route(
        &credits,
        &auth_context,
        "fetchfast",
        Some(&payload.source),
        None,
    )
    .await?;
    let data = fetcher.get_fast_data_with_receipt(&payload.source).await?;
    Ok(Json(data))
}

pub async fn get_receipt(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Path(id): Path<String>,
) -> Result<Json<Receipt>, FetchError> {
    validate_receipt_id(&id)?;
    debit_product_route(&credits, &auth_context, "receipt", None, Some(&id)).await?;
    fetcher
        .get_receipt(&id)
        .map(Json)
        .ok_or_else(|| FetchError::NotFound(format!("Receipt not found: {id}")))
}

pub async fn snapshot_source(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<crate::snapshot_upload::SnapshotPayload>, FetchError> {
    validate_source_url(&payload.source)?;
    egress.validate_source(&payload.source).await?;
    debit_product_route(
        &credits,
        &auth_context,
        "snapshot",
        Some(&payload.source),
        None,
    )
    .await?;
    let snapshot = fetcher.snapshot_with_receipt(&payload.source).await?;
    Ok(Json(snapshot))
}

#[axum::debug_handler]
pub async fn fetch_unblock(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
    ApiJson(payload): ApiJson<FetchRequest>,
) -> Result<Json<Value>, FetchError> {
    validate_source_url(&payload.source)?;
    egress.validate_source(&payload.source).await?;
    debit_product_route(
        &credits,
        &auth_context,
        "fetchunblock",
        Some(&payload.source),
        None,
    )
    .await?;
    let data = fetcher.unblocker(&payload.source).await?;
    Ok(Json(data))
}

async fn debit_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
    source_url: Option<&str>,
    subject_id: Option<&str>,
) -> Result<(), FetchError> {
    match credits
        .debit_product_request(auth_context, route, source_url, subject_id)
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
