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
use crate::fetch::Fetcher;
use crate::types::{
    FetchWithReceipt, ProductMode, ProductRequest, ProductResponse, ProductRoute,
    ProvenanceConsent, Receipt, validate_idempotency_key, validate_receipt_id,
};
use serde::Deserialize;
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
    #[serde(default)]
    pub provenance: Option<ProvenanceConsent>,
}

pub async fn fetch_post(
    State(fetcher): State<Arc<Fetcher>>,
    Extension(auth_context): Extension<ResolverAuthContext>,
    Extension(credits): Extension<Arc<ResolverCreditsClient>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<ProductRequest>,
) -> Result<Json<ProductResponse>, FetchError> {
    payload.validate_for(ProductRoute::Scrape)?;
    let logical_request = Fetcher::product_billing_context(&payload, ProductRoute::Scrape)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let source_url = payload.source.clone();
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        ProductRoute::Scrape.as_str(),
        source_url.as_deref(),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(payload, ProductRoute::Scrape, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        ProductRoute::Scrape.as_str(),
        source_url.as_deref(),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_auth(pending, Some(&auth_context))
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
    payload.validate_for(ProductRoute::Crawl)?;
    let logical_request = Fetcher::product_billing_context(&payload, ProductRoute::Crawl)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let source_url = payload.source.clone();
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        ProductRoute::Crawl.as_str(),
        source_url.as_deref(),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(payload, ProductRoute::Crawl, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        ProductRoute::Crawl.as_str(),
        source_url.as_deref(),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_auth(pending, Some(&auth_context))
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
    payload.validate_for(ProductRoute::Map)?;
    let logical_request = Fetcher::product_billing_context(&payload, ProductRoute::Map)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let source_url = payload.source.clone();
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        ProductRoute::Map.as_str(),
        source_url.as_deref(),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(payload, ProductRoute::Map, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        ProductRoute::Map.as_str(),
        source_url.as_deref(),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_auth(pending, Some(&auth_context))
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
    payload.validate_for(ProductRoute::Search)?;
    let logical_request = Fetcher::product_billing_context(&payload, ProductRoute::Search)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        ProductRoute::Search.as_str(),
        None,
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(payload, ProductRoute::Search, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        ProductRoute::Search.as_str(),
        None,
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_auth(pending, Some(&auth_context))
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
    payload.validate_for(ProductRoute::Extract)?;
    let logical_request = Fetcher::product_billing_context(&payload, ProductRoute::Extract)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let source_url = payload.source.clone();
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        ProductRoute::Extract.as_str(),
        source_url.as_deref(),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(payload, ProductRoute::Extract, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        ProductRoute::Extract.as_str(),
        source_url.as_deref(),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_auth(pending, Some(&auth_context))
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
    payload.validate_for(ProductRoute::Screenshot)?;
    let logical_request = Fetcher::product_billing_context(&payload, ProductRoute::Screenshot)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let source_url = payload.source.clone();
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        ProductRoute::Screenshot.as_str(),
        source_url.as_deref(),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(payload, ProductRoute::Screenshot, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        ProductRoute::Screenshot.as_str(),
        source_url.as_deref(),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_auth(pending, Some(&auth_context))
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
    let mut request = ProductRequest::fast(&payload.source);
    request.provenance = payload.provenance;
    request.validate_for(ProductRoute::Scrape)?;
    let logical_request = Fetcher::product_billing_context(&request, ProductRoute::Scrape)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        "fetchfast",
        Some(&payload.source),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(request, ProductRoute::Scrape, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        "fetchfast",
        Some(&payload.source),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_receipt_with_auth(pending, Some(&auth_context))
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
    let idempotency_key = request_idempotency_key(&headers)?;
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        "receipt",
        None,
        Some(&id),
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let receipt = fetcher
        .get_receipt(&id, Some(&auth_context))
        .await?
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
    let request = Fetcher::snapshot_request(&payload.source, payload.provenance);
    request.validate_for(ProductRoute::Snapshot)?;
    let logical_request = Fetcher::product_billing_context(&request, ProductRoute::Snapshot)?;
    let idempotency_key = request_idempotency_key(&headers)?;
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
        .prepare_snapshot_with_auth(request, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        "snapshot",
        Some(&payload.source),
        authorization,
    )
    .await?;
    let snapshot = fetcher
        .finalize_snapshot_with_auth(pending, Some(&auth_context))
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
) -> Result<Json<FetchWithReceipt>, FetchError> {
    let mut request = ProductRequest::fast(&payload.source);
    request.mode = ProductMode::Unblock;
    request.receipt = Some(true);
    request.provenance = payload.provenance;
    request.validate_for(ProductRoute::Unblock)?;
    let logical_request = Fetcher::product_billing_context(&request, ProductRoute::Unblock)?;
    let idempotency_key = request_idempotency_key(&headers)?;
    let authorization = preflight_product_route(
        &credits,
        &auth_context,
        "fetchunblock",
        Some(&payload.source),
        None,
        &logical_request,
        idempotency_key.as_deref(),
    )
    .await?;
    let pending = fetcher
        .prepare_product_fetch_with_auth(request, ProductRoute::Unblock, Some(&auth_context))
        .await?;
    capture_product_route(
        &credits,
        &auth_context,
        "fetchunblock",
        Some(&payload.source),
        authorization,
    )
    .await?;
    let data = fetcher
        .finalize_product_fetch_with_receipt_with_auth(pending, Some(&auth_context))
        .await?;
    Ok(Json(data))
}

async fn preflight_product_route(
    credits: &ResolverCreditsClient,
    auth_context: &ResolverAuthContext,
    route: &str,
    source_url: Option<&str>,
    subject_id: Option<&str>,
    logical_request: &serde_json::Value,
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
        Err(err) if err.is_idempotency_already_finalized() => Err(FetchError::IdempotencyConflict(
            "Idempotency-Key was already finalized; the prior result is not available for replay"
                .to_string(),
        )),
        Err(err) if err.is_idempotency_conflict() => Err(FetchError::IdempotencyConflict(
            "Idempotency-Key was already used for a different logical request".to_string(),
        )),
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
            if outcome.permits_work() {
                Ok(())
            } else {
                Err(FetchError::Credits(
                    "credit debit response did not enforce the request".to_string(),
                ))
            }
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
        Err(err) if err.is_idempotency_conflict() => Err(FetchError::IdempotencyConflict(
            "Idempotency-Key was already used for a different logical request".to_string(),
        )),
        Err(err) => Err(FetchError::Credits(err.to_string())),
    }
}

fn request_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, FetchError> {
    let mut values = headers.get_all("idempotency-key").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(FetchError::BadRequest(
            "Idempotency-Key must be sent at most once".to_string(),
        ));
    }
    let value = value.to_str().map_err(|_| {
        FetchError::BadRequest("Idempotency-Key must contain valid ASCII".to_string())
    })?;
    validate_idempotency_key(Some(value))?;
    Ok(Some(value.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn idempotency_header_is_validated_and_normalized() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", HeaderValue::from_static("retry-123"));
        assert_eq!(
            request_idempotency_key(&headers).unwrap().as_deref(),
            Some("retry-123")
        );

        headers.insert("idempotency-key", HeaderValue::from_static("bad key"));
        assert!(request_idempotency_key(&headers).is_err());

        let mut duplicate = HeaderMap::new();
        duplicate.append("idempotency-key", HeaderValue::from_static("one"));
        duplicate.append("idempotency-key", HeaderValue::from_static("two"));
        assert!(request_idempotency_key(&duplicate).is_err());
    }
}
