//! Spider execution layer for product routes and receipts.

use crate::auth::ResolverAuthContext;
use crate::errors::FetchError;
use crate::provenance::{ProvenanceClient, ResolverFetchEvidence};
use crate::receipt_store::{ReceiptOwner, ReceiptStore, ReceiptStoreFactory, env_bool};
use crate::snapshot_upload::SnapshotPayload;
use crate::types::{
    FetchWithReceipt, FormatSelection, ProductFormat, ProductMode, ProductProxy, ProductRequest,
    ProductResponse, ProductRoute, ProvenanceConsent, Receipt,
};
use serde_json::{Value, json};
use spider_client::{
    CSSSelector, Delay, Engine, IdleNetwork, ProxyType, RequestParams, RequestType, ReturnFormat,
    ReturnFormatHandling, SearchRequestParams, Selector, Spider, Timeout, WaitFor,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub struct Fetcher {
    spider: Spider,
    provenance: Option<ProvenanceClient>,
    receipts: Arc<dyn ReceiptStore>,
}

pub struct PendingProductFetch {
    payload: ProductRequest,
    route: ProductRoute,
    mode: ProductMode,
    should_receipt: bool,
    receipt_owner: Option<ReceiptOwner>,
    data: Value,
}

pub struct PendingSnapshot {
    payload: ProductRequest,
    owner: ReceiptOwner,
    data: Value,
    request_type: &'static str,
    proxy: Option<String>,
}

impl Fetcher {
    pub fn from_receipt_store_factory(
        factory: &dyn ReceiptStoreFactory,
    ) -> Result<Self, FetchError> {
        dotenvy::dotenv().ok();
        let receipts = factory
            .create()
            .map_err(|err| FetchError::Http(err.to_string()))?;
        Self::with_receipt_store(receipts)
    }

    pub fn with_receipt_store(receipts: Arc<dyn ReceiptStore>) -> Result<Self, FetchError> {
        dotenvy::dotenv().ok();
        let explicit_memory_exception = env_bool("LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS", false)
            .map_err(|err| FetchError::Http(err.to_string()))?;
        if !receipt_store_is_allowed(receipts.is_shared_durable(), explicit_memory_exception) {
            return Err(FetchError::Http(
                "non-durable receipt storage requires the explicit LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true exception in every environment"
                    .to_string(),
            ));
        }
        let key = std::env::var("LIVY_RESOLVER_KEY")
            .or_else(|_| std::env::var("SPIDER_API_KEY"))
            .or_else(|_| std::env::var("SPIDER_KEY"))
            .or_else(|_| std::env::var("LIVY_KEY"))
            .map_err(|_| FetchError::Http("LIVY_RESOLVER_KEY must be set".to_string()))?;
        let spider = Spider::new(Some(key)).map_err(|err| FetchError::Http(err.to_string()))?;
        let provenance = ProvenanceClient::from_env()
            .map_err(|err| FetchError::Http(format!("invalid provenance configuration: {err}")))?;
        if !receipts.is_shared_durable() {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "receipt_store_single_replica",
                    "durable": false,
                })
            );
        }
        Ok(Fetcher {
            spider,
            provenance,
            receipts,
        })
    }

    pub fn product_billing_context(
        payload: &ProductRequest,
        route: ProductRoute,
    ) -> Result<Value, FetchError> {
        payload.validate_for(route)?;
        let mode = Self::resolve_mode(payload.mode, route);
        let timeout_secs = payload.timeout_secs.unwrap_or(match mode {
            ProductMode::Unblock => 50,
            ProductMode::Browser | ProductMode::Crawl | ProductMode::Screenshot => 45,
            _ => 25,
        });
        let should_receipt = payload.receipt.unwrap_or(matches!(
            mode,
            ProductMode::Auto | ProductMode::Fast | ProductMode::Extract
        ));
        let (operation, upstream_parameters) = match route {
            ProductRoute::Search => {
                let query = payload
                    .query
                    .as_deref()
                    .ok_or_else(|| FetchError::BadRequest("search requires `query`".to_string()))?;
                (
                    "search",
                    Self::serialized_upstream_parameters(Self::search_request_params(
                        query, payload,
                    ))?,
                )
            }
            ProductRoute::Screenshot => {
                let mut params = Self::request_params(payload, ProductMode::Screenshot);
                params.return_format = Some(ReturnFormatHandling::Single(ReturnFormat::Screenshot));
                ("screenshot", Self::serialized_upstream_parameters(params)?)
            }
            ProductRoute::Snapshot => (
                "scrape_snapshot",
                Self::serialized_upstream_parameters(Self::request_params(
                    payload,
                    ProductMode::Screenshot,
                ))?,
            ),
            ProductRoute::Map => (
                "links",
                Self::serialized_upstream_parameters(Self::request_params(
                    payload,
                    ProductMode::Map,
                ))?,
            ),
            ProductRoute::Crawl => (
                "crawl",
                Self::serialized_upstream_parameters(Self::request_params(
                    payload,
                    ProductMode::Crawl,
                ))?,
            ),
            ProductRoute::Unblock => (
                "unblock",
                Self::serialized_upstream_parameters(Self::request_params(
                    payload,
                    ProductMode::Unblock,
                ))?,
            ),
            ProductRoute::Extract | ProductRoute::Scrape if mode == ProductMode::Unblock => (
                "unblock",
                Self::serialized_upstream_parameters(Self::request_params(
                    payload,
                    ProductMode::Unblock,
                ))?,
            ),
            ProductRoute::Extract | ProductRoute::Scrape => (
                "scrape",
                Self::serialized_upstream_parameters(Self::request_params(payload, payload.mode))?,
            ),
        };

        Ok(json!({
            "contract": "livy-resolver-logical-request-v1",
            "kind": "product",
            "route": route.as_str(),
            "operation": operation,
            "resolved_mode": mode,
            "outer_timeout_secs": timeout_secs,
            "receipt": should_receipt,
            "source": payload.source,
            "query": payload.query,
            "upstream_parameters": upstream_parameters,
            "validated_request": payload,
        }))
    }

    pub fn receipt_billing_context(receipt_id: &str) -> Value {
        json!({
            "contract": "livy-resolver-logical-request-v1",
            "kind": "receipt_read",
            "route": "receipt",
            "receipt_id": receipt_id,
        })
    }

    pub async fn prepare_product_fetch_with_auth(
        &self,
        payload: ProductRequest,
        route: ProductRoute,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<PendingProductFetch, FetchError> {
        payload.validate_for(route)?;
        let mode = Self::resolve_mode(payload.mode, route);
        let should_receipt = payload.receipt.unwrap_or(matches!(
            mode,
            ProductMode::Auto | ProductMode::Fast | ProductMode::Extract
        ));
        let receipt_owner = should_receipt
            .then(|| ReceiptOwner::from_auth_context(auth_context))
            .transpose()
            .map_err(|err| FetchError::Http(err.to_string()))?;
        let timeout_secs = payload.timeout_secs.unwrap_or(match mode {
            ProductMode::Unblock => 50,
            ProductMode::Browser | ProductMode::Crawl | ProductMode::Screenshot => 45,
            _ => 25,
        });

        let data = match route {
            ProductRoute::Search => {
                let query = payload
                    .query
                    .as_deref()
                    .ok_or_else(|| FetchError::BadRequest("search requires `query`".to_string()))?;
                self.search(query, &payload, timeout_secs).await?
            }
            ProductRoute::Map => {
                let source = Self::source(&payload)?;
                self.map(source, &payload, timeout_secs).await?
            }
            ProductRoute::Crawl => {
                let source = Self::source(&payload)?;
                self.crawl(source, &payload, timeout_secs).await?
            }
            ProductRoute::Screenshot => {
                let source = Self::source(&payload)?;
                self.screenshot(source, &payload, timeout_secs).await?
            }
            ProductRoute::Snapshot => {
                let source = Self::source(&payload)?;
                self.scrape(source, &payload, timeout_secs).await?
            }
            ProductRoute::Unblock => {
                let source = Self::source(&payload)?;
                self.unblock(source, &payload, timeout_secs).await?
            }
            ProductRoute::Extract | ProductRoute::Scrape => {
                let source = Self::source(&payload)?;
                if mode == ProductMode::Unblock {
                    self.unblock(source, &payload, timeout_secs).await?
                } else {
                    self.scrape(source, &payload, timeout_secs).await?
                }
            }
        };

        let data = Self::ensure_spider_success(data)?;
        Ok(PendingProductFetch {
            payload,
            route,
            mode,
            should_receipt,
            receipt_owner,
            data,
        })
    }

    pub async fn finalize_product_fetch_with_auth(
        &self,
        pending: PendingProductFetch,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<ProductResponse, FetchError> {
        let PendingProductFetch {
            payload,
            route,
            mode,
            should_receipt,
            receipt_owner,
            data,
        } = pending;
        let (receipt_id, receipt) = if should_receipt {
            let source = payload
                .source
                .as_deref()
                .or(payload.query.as_deref())
                .unwrap_or("unknown");
            let owner = receipt_owner
                .as_ref()
                .ok_or_else(|| FetchError::Http("receipt owner was not resolved".to_string()))?;
            let receipt = self
                .store_receipt(
                    owner,
                    source,
                    &data,
                    mode.receipt_label(),
                    Self::request_type_name(mode),
                    Self::proxy_name(&payload, mode),
                )
                .await?;
            (Some(receipt.id.clone()), Some(receipt))
        } else {
            (None, None)
        };

        let (provenance, provenance_error) = self
            .provenance_for(&payload, route, mode, &data, receipt.as_ref(), auth_context)
            .await;

        Ok(ProductResponse {
            route: route.as_str().to_string(),
            mode,
            receipt_id,
            receipt,
            data,
            provenance,
            provenance_error,
        })
    }

    pub async fn finalize_product_fetch_with_receipt_with_auth(
        &self,
        pending: PendingProductFetch,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<FetchWithReceipt, FetchError> {
        let response = self
            .finalize_product_fetch_with_auth(pending, auth_context)
            .await?;
        let receipt = response
            .receipt
            .ok_or_else(|| FetchError::Http("receipt was not created".to_string()))?;

        Ok(FetchWithReceipt {
            receipt_id: receipt.id.clone(),
            receipt,
            data: response.data,
            provenance: response.provenance,
            provenance_error: response.provenance_error,
        })
    }

    pub fn snapshot_request(source: &str, provenance: Option<ProvenanceConsent>) -> ProductRequest {
        let mut payload = ProductRequest::fast(source);
        payload.mode = ProductMode::Screenshot;
        payload.formats = Some(vec![ProductFormat::Raw, ProductFormat::Screenshot]);
        payload.receipt = Some(true);
        payload.stealth = Some(true);
        payload.fingerprint = Some(true);
        payload.provenance = provenance;
        payload
    }

    pub async fn prepare_snapshot_with_auth(
        &self,
        payload: ProductRequest,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<PendingSnapshot, FetchError> {
        payload.validate_for(ProductRoute::Snapshot)?;
        let source = payload.require_source()?;
        let owner = ReceiptOwner::from_auth_context(auth_context)
            .map_err(|err| FetchError::Http(err.to_string()))?;
        let params = Self::request_params(&payload, ProductMode::Screenshot);
        let request_type = Self::request_type_name_from_params(&params);
        let proxy = Self::proxy_name_from_params(&params).map(str::to_string);

        let data = self
            .spider
            .scrape_url(source, Some(params), "application/json")
            .await
            .map_err(FetchError::UnableFetch)?;
        let crawl = Self::ensure_spider_success(Self::normalize_value(data)?)?;
        SnapshotPayload::validate_spider_response(&crawl).map_err(FetchError::Snapshot)?;
        Ok(PendingSnapshot {
            payload,
            owner,
            data: crawl,
            request_type,
            proxy,
        })
    }

    pub async fn finalize_snapshot_with_auth(
        &self,
        pending: PendingSnapshot,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<SnapshotPayload, FetchError> {
        let PendingSnapshot {
            payload,
            owner,
            data: crawl,
            request_type,
            proxy,
        } = pending;
        let source = payload.require_source()?;
        let receipt = self
            .store_receipt(
                &owner,
                source,
                &crawl,
                ProductMode::Screenshot.receipt_label(),
                request_type,
                proxy.as_deref(),
            )
            .await?;
        let (provenance, provenance_error) = self
            .provenance_for(
                &payload,
                ProductRoute::Snapshot,
                ProductMode::Screenshot,
                &crawl,
                Some(&receipt),
                auth_context,
            )
            .await;

        SnapshotPayload::from_spider_response(
            source,
            receipt.id,
            crawl,
            provenance,
            provenance_error,
        )
        .map_err(FetchError::Snapshot)
    }

    async fn provenance_for(
        &self,
        payload: &ProductRequest,
        route: ProductRoute,
        mode: ProductMode,
        data: &Value,
        receipt: Option<&Receipt>,
        auth_context: Option<&ResolverAuthContext>,
    ) -> (Option<crate::provenance::ProvenanceResult>, Option<String>) {
        let Some(provenance) = self.provenance.as_ref() else {
            return (None, None);
        };

        match provenance
            .attest_fetch(
                ResolverFetchEvidence {
                    payload,
                    route,
                    mode,
                    data,
                    receipt,
                },
                auth_context,
            )
            .await
        {
            Ok(result) => (Some(result), None),
            Err(_) => {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "event": "provenance_attestation_failed",
                        "request_id": crate::security::current_request_id(),
                        "error_kind": "provenance_error",
                    })
                );
                (None, Some("provenance attestation unavailable".to_string()))
            }
        }
    }

    async fn scrape(
        &self,
        source: &str,
        payload: &ProductRequest,
        timeout_secs: u64,
    ) -> Result<Value, FetchError> {
        let params = Self::request_params(payload, payload.mode);
        Self::normalize_value(
            tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                self.spider
                    .scrape_url(source, Some(params), "application/json"),
            )
            .await
            .map_err(|_| FetchError::Timeout(format!("after {timeout_secs}s")))?
            .map_err(FetchError::UnableFetch)?,
        )
    }

    async fn unblock(
        &self,
        source: &str,
        payload: &ProductRequest,
        timeout_secs: u64,
    ) -> Result<Value, FetchError> {
        let params = Self::request_params(payload, ProductMode::Unblock);
        Self::normalize_value(
            tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                self.spider
                    .unblock_url(source, Some(params), "application/json"),
            )
            .await
            .map_err(|_| FetchError::Timeout(format!("after {timeout_secs}s")))?
            .map_err(FetchError::UnableFetch)?,
        )
    }

    async fn crawl(
        &self,
        source: &str,
        payload: &ProductRequest,
        timeout_secs: u64,
    ) -> Result<Value, FetchError> {
        let params = Self::request_params(payload, ProductMode::Crawl);
        Self::normalize_value(
            tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                self.spider.crawl_url(
                    source,
                    Some(params),
                    false,
                    "application/json",
                    None::<fn(Value)>,
                ),
            )
            .await
            .map_err(|_| FetchError::Timeout(format!("after {timeout_secs}s")))?
            .map_err(FetchError::UnableFetch)?,
        )
    }

    async fn map(
        &self,
        source: &str,
        payload: &ProductRequest,
        timeout_secs: u64,
    ) -> Result<Value, FetchError> {
        let params = Self::request_params(payload, ProductMode::Map);
        Self::normalize_value(
            tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                self.spider
                    .links(source, Some(params), false, "application/json"),
            )
            .await
            .map_err(|_| FetchError::Timeout(format!("after {timeout_secs}s")))?
            .map_err(FetchError::UnableFetch)?,
        )
    }

    async fn screenshot(
        &self,
        source: &str,
        payload: &ProductRequest,
        timeout_secs: u64,
    ) -> Result<Value, FetchError> {
        let mut params = Self::request_params(payload, ProductMode::Screenshot);
        params.return_format = Some(ReturnFormatHandling::Single(ReturnFormat::Screenshot));
        Self::normalize_value(
            tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                self.spider
                    .screenshot(source, Some(params), false, "application/json"),
            )
            .await
            .map_err(|_| FetchError::Timeout(format!("after {timeout_secs}s")))?
            .map_err(FetchError::UnableFetch)?,
        )
    }

    async fn search(
        &self,
        query: &str,
        payload: &ProductRequest,
        timeout_secs: u64,
    ) -> Result<Value, FetchError> {
        let params = Self::search_request_params(query, payload);
        Self::normalize_value(
            tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                self.spider
                    .search(query, Some(params), false, "application/json"),
            )
            .await
            .map_err(|_| FetchError::Timeout(format!("after {timeout_secs}s")))?
            .map_err(FetchError::UnableFetch)?,
        )
    }

    /// Resolve the route into the mode that should be executed.
    fn resolve_mode(mode: ProductMode, route: ProductRoute) -> ProductMode {
        match route {
            ProductRoute::Crawl => ProductMode::Crawl,
            ProductRoute::Map => ProductMode::Map,
            ProductRoute::Search => ProductMode::Search,
            ProductRoute::Extract => ProductMode::Extract,
            ProductRoute::Screenshot => ProductMode::Screenshot,
            ProductRoute::Snapshot => ProductMode::Screenshot,
            ProductRoute::Unblock => ProductMode::Unblock,
            ProductRoute::Scrape => mode,
        }
    }

    /// Return the required source URL from a product request.
    fn source(payload: &ProductRequest) -> Result<&str, FetchError> {
        payload.require_source()
    }

    /// Build Spider request params from product options.
    fn request_params(payload: &ProductRequest, mode: ProductMode) -> RequestParams {
        RequestParams {
            request: Some(Self::request_type(mode)),
            return_format: Some(Self::return_format(payload, mode)),
            proxy: Self::proxy_type(payload, mode),
            proxy_enabled: Some(matches!(mode, ProductMode::Unblock)),
            limit: payload.limit,
            depth: payload.depth,
            cache: payload.cache,
            metadata: payload.metadata,
            return_headers: payload.return_headers,
            return_page_links: payload.return_page_links,
            return_cookies: payload.return_cookies,
            country_code: payload.country_code.clone(),
            locale: payload.locale.clone(),
            user_agent: payload.user_agent.clone(),
            headers: payload.headers.clone(),
            cookies: payload.cookies.clone(),
            root_selector: payload.root_selector.clone(),
            css_extraction_map: Self::css_extraction_map(payload),
            whitelist: payload.whitelist.clone(),
            blacklist: payload.blacklist.clone(),
            tld: payload.tld,
            subdomains: payload.subdomains,
            external_domains: payload.external_domains.clone(),
            sitemap: payload.sitemap,
            respect_robots: payload.respect_robots,
            stealth: Some(
                payload
                    .stealth
                    .unwrap_or(matches!(mode, ProductMode::Unblock)),
            ),
            fingerprint: Some(
                payload
                    .fingerprint
                    .unwrap_or(matches!(mode, ProductMode::Unblock)),
            ),
            scroll: payload.scroll.or({
                if matches!(mode, ProductMode::Unblock) {
                    Some(1)
                } else {
                    None
                }
            }),
            wait_for: Self::wait_for(payload),
            disable_intercept: payload.disable_intercept,
            disable_hints: payload.disable_hints,
            lite_mode: payload.lite_mode,
            max_credits_per_page: payload.max_credits_per_page,
            request_timeout: payload
                .request_timeout_secs
                .or(Some(Self::default_request_timeout(mode))),
            crawl_timeout: Some(Timeout {
                secs: payload
                    .crawl_timeout_secs
                    .unwrap_or_else(|| Self::default_crawl_timeout(mode)),
                nanos: 0,
            }),
            readability: Some(
                payload
                    .readability
                    .unwrap_or(!matches!(mode, ProductMode::Raw)),
            ),
            ..Default::default()
        }
    }

    fn search_request_params(query: &str, payload: &ProductRequest) -> SearchRequestParams {
        SearchRequestParams {
            base: Self::request_params(payload, ProductMode::Search),
            search: query.to_string(),
            search_limit: payload.search_limit.or(payload.limit),
            fetch_page_content: payload.fetch_page_content,
            num: payload.limit,
            quick_search: payload.quick_search,
            engine: payload.engine.as_deref().and_then(parse_engine),
            ..Default::default()
        }
    }

    fn serialized_upstream_parameters<T: serde::Serialize>(params: T) -> Result<Value, FetchError> {
        let mut value = serde_json::to_value(params)?;
        Self::normalize_unordered_upstream_values(&mut value);
        Ok(value)
    }

    fn normalize_unordered_upstream_values(value: &mut Value) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    Self::normalize_unordered_upstream_values(value);
                    if key == "return_format"
                        && let Some(formats) = value.as_array_mut()
                    {
                        formats.sort_by_key(Value::to_string);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    Self::normalize_unordered_upstream_values(value);
                }
            }
            _ => {}
        }
    }

    /// Select the Spider request type for a product mode.
    fn request_type(mode: ProductMode) -> RequestType {
        match mode {
            ProductMode::Browser | ProductMode::Unblock | ProductMode::Screenshot => {
                RequestType::Chrome
            }
            ProductMode::Raw => RequestType::Http,
            _ => RequestType::SmartMode,
        }
    }

    /// Human-readable request type for receipts.
    fn request_type_name(mode: ProductMode) -> &'static str {
        match Self::request_type(mode) {
            RequestType::Http => "http",
            RequestType::Chrome => "chrome",
            RequestType::SmartMode => "smart",
        }
    }

    fn request_type_name_from_params(params: &RequestParams) -> &'static str {
        match params.request.as_ref() {
            Some(RequestType::Http) => "http",
            Some(RequestType::Chrome) => "chrome",
            Some(RequestType::SmartMode) => "smart",
            None => "unspecified",
        }
    }

    /// Select upstream proxy routing for a product mode.
    fn proxy_type(payload: &ProductRequest, mode: ProductMode) -> Option<ProxyType> {
        match payload.proxy.unwrap_or(ProductProxy::Auto) {
            ProductProxy::Auto => match mode {
                ProductMode::Fast
                | ProductMode::Auto
                | ProductMode::Extract
                | ProductMode::Unblock => Some(ProxyType::Isp),
                _ => None,
            },
            ProductProxy::None => None,
            ProductProxy::Isp => Some(ProxyType::Isp),
            ProductProxy::Residential => Some(ProxyType::Residential),
            ProductProxy::Mobile => Some(ProxyType::Mobile),
        }
    }

    /// Human-readable proxy name for receipts.
    fn proxy_name(payload: &ProductRequest, mode: ProductMode) -> Option<&'static str> {
        Self::proxy_type(payload, mode).map(|proxy| proxy.as_str())
    }

    fn proxy_name_from_params(params: &RequestParams) -> Option<&str> {
        params.proxy.as_ref().map(ProxyType::as_str)
    }

    /// Default upstream request timeout for each mode.
    fn default_request_timeout(mode: ProductMode) -> u8 {
        match mode {
            ProductMode::Unblock | ProductMode::Browser | ProductMode::Screenshot => 30,
            _ => 20,
        }
    }

    /// Default upstream crawl timeout for each mode.
    fn default_crawl_timeout(mode: ProductMode) -> u64 {
        match mode {
            ProductMode::Unblock | ProductMode::Browser | ProductMode::Screenshot => 45,
            ProductMode::Crawl => 45,
            _ => 25,
        }
    }

    /// Convert product format options into Spider format handling.
    fn return_format(payload: &ProductRequest, mode: ProductMode) -> ReturnFormatHandling {
        let formats = payload
            .formats
            .clone()
            .or_else(|| match payload.format.clone() {
                Some(FormatSelection::One(format)) => Some(vec![format]),
                Some(FormatSelection::Many(formats)) => Some(formats),
                None => None,
            });

        let formats = formats.unwrap_or_else(|| match mode {
            ProductMode::Raw => vec![ProductFormat::Raw],
            ProductMode::Screenshot => vec![ProductFormat::Screenshot],
            _ => vec![ProductFormat::Markdown],
        });

        if formats.len() == 1 {
            ReturnFormatHandling::Single(Self::to_spider_format(formats[0]))
        } else {
            ReturnFormatHandling::Multi(
                formats
                    .into_iter()
                    .map(Self::to_spider_format)
                    .collect::<HashSet<_>>(),
            )
        }
    }

    /// Convert product format enum into Spider format enum.
    fn to_spider_format(format: ProductFormat) -> ReturnFormat {
        match format {
            ProductFormat::Raw => ReturnFormat::Raw,
            ProductFormat::Markdown => ReturnFormat::Markdown,
            ProductFormat::Commonmark => ReturnFormat::Commonmark,
            ProductFormat::Html2text => ReturnFormat::Html2text,
            ProductFormat::Text => ReturnFormat::Text,
            ProductFormat::Screenshot => ReturnFormat::Screenshot,
            ProductFormat::Xml => ReturnFormat::Xml,
            ProductFormat::Bytes => ReturnFormat::Bytes,
        }
    }

    /// Convert simple selector maps into Spider CSS extraction maps.
    fn css_extraction_map(payload: &ProductRequest) -> Option<HashMap<String, Vec<CSSSelector>>> {
        payload.selectors.as_ref().map(|selectors| {
            let values = selectors
                .iter()
                .map(|(name, selectors)| CSSSelector {
                    name: name.clone(),
                    selectors: selectors.clone(),
                })
                .collect::<Vec<_>>();
            HashMap::from([("*".to_string(), values)])
        })
    }

    /// Build wait conditions for browser-capable requests.
    fn wait_for(payload: &ProductRequest) -> Option<WaitFor> {
        if payload.wait_ms.is_none() && payload.wait_selector.is_none() {
            return None;
        }

        Some(WaitFor {
            delay: payload.wait_ms.map(|millis| Delay {
                timeout: Timeout {
                    secs: millis / 1000,
                    nanos: ((millis % 1000) * 1_000_000) as u32,
                },
            }),
            selector: payload.wait_selector.as_ref().map(|selector| Selector {
                selector: selector.clone(),
                timeout: Timeout { secs: 30, nanos: 0 },
            }),
            idle_network: Some(IdleNetwork {
                timeout: Timeout { secs: 2, nanos: 0 },
            }),
            ..Default::default()
        })
    }

    /// Convert string-wrapped Spider JSON into normal JSON values.
    fn normalize_value(value: Value) -> Result<Value, FetchError> {
        match value {
            Value::String(s) => Ok(serde_json::from_str::<Value>(&s)?),
            other => Ok(other),
        }
    }

    fn ensure_spider_success(value: Value) -> Result<Value, FetchError> {
        let first = value.as_array().and_then(|items| items.first());
        let status = first
            .and_then(|item| item.get("status"))
            .and_then(Value::as_i64);
        let error = first
            .and_then(|item| item.get("error"))
            .and_then(Value::as_str)
            .filter(|error| !error.is_empty());

        if matches!(status, Some(0)) || error.is_some() {
            return Err(FetchError::Upstream(
                error
                    .unwrap_or("Spider returned an unsuccessful fetch status")
                    .to_string(),
            ));
        }

        Ok(value)
    }

    pub async fn get_receipt(
        &self,
        id: &str,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<Option<Receipt>, FetchError> {
        let owner = ReceiptOwner::from_auth_context(auth_context)
            .map_err(|err| FetchError::Http(err.to_string()))?;
        self.receipts
            .get(&owner, id)
            .await
            .map_err(|err| FetchError::Http(err.to_string()))
    }

    async fn store_receipt(
        &self,
        owner: &ReceiptOwner,
        source: &str,
        value: &Value,
        mode: &str,
        request_type: &str,
        proxy: Option<&str>,
    ) -> Result<Receipt, FetchError> {
        let id = new_receipt_id();
        let first = value.as_array().and_then(|items| items.first());
        let status = first
            .and_then(|item| item.get("status"))
            .and_then(Value::as_i64);
        let error = first
            .and_then(|item| item.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let duration_elapsed_ms = first
            .and_then(|item| item.get("duration_elasped_ms"))
            .and_then(Value::as_u64);
        let content_bytes = first
            .and_then(|item| item.get("content"))
            .and_then(Value::as_str)
            .map(str::len);
        let total_cost = first
            .and_then(|item| item.get("costs"))
            .and_then(|costs| costs.get("total_cost_formatted"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let receipt = Receipt {
            id,
            source_url: source.to_string(),
            mode: mode.to_string(),
            request_type: request_type.to_string(),
            proxy: proxy.map(str::to_string),
            status,
            error,
            duration_elapsed_ms,
            content_bytes,
            total_cost,
            created_at_unix_ms: Self::now_unix_ms(),
            demo_message: "amazing job".to_string(),
        };

        self.receipts
            .put(owner, receipt.clone())
            .await
            .map_err(|err| FetchError::Http(err.to_string()))?;

        Ok(receipt)
    }

    fn now_unix_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    }
}

fn new_receipt_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn receipt_store_is_allowed(is_shared_durable: bool, explicit_memory_exception: bool) -> bool {
    is_shared_durable || explicit_memory_exception
}

/// Parse product search engine strings.
fn parse_engine(engine: &str) -> Option<Engine> {
    match engine.to_ascii_lowercase().as_str() {
        "google" => Some(Engine::Google),
        "brave" => Some(Engine::Brave),
        "all" => Some(Engine::All),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Fetcher, new_receipt_id, receipt_store_is_allowed};
    use crate::types::{ProductFormat, ProductMode, ProductRequest, ProductRoute};

    #[test]
    fn receipt_ids_are_random_uuid_values() {
        let first = new_receipt_id();
        let second = new_receipt_id();

        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn non_durable_receipts_always_require_an_explicit_exception() {
        assert!(receipt_store_is_allowed(true, false));
        assert!(receipt_store_is_allowed(true, true));
        assert!(!receipt_store_is_allowed(false, false));
        assert!(receipt_store_is_allowed(false, true));
    }

    #[test]
    fn snapshot_billing_and_receipt_metadata_use_the_exact_execution_params() {
        let payload = Fetcher::snapshot_request("https://example.com/private", None);
        let executed = Fetcher::request_params(&payload, ProductMode::Screenshot);
        let recorded = Fetcher::product_billing_context(&payload, ProductRoute::Snapshot).unwrap();

        assert_eq!(
            recorded["upstream_parameters"],
            Fetcher::serialized_upstream_parameters(&executed).unwrap()
        );
        assert_eq!(recorded["route"], "snapshot");
        assert_eq!(recorded["operation"], "scrape_snapshot");
        assert_eq!(Fetcher::request_type_name_from_params(&executed), "chrome");
        assert_eq!(
            Fetcher::proxy_name_from_params(&executed),
            executed
                .proxy
                .as_ref()
                .map(spider_client::ProxyType::as_str)
        );
        assert_eq!(
            payload.formats,
            Some(vec![ProductFormat::Raw, ProductFormat::Screenshot])
        );
    }

    #[test]
    fn billing_context_covers_search_effective_options_and_receipt_identity() {
        let mut search = ProductRequest::legacy("https://unused.example");
        search.source = None;
        search.query = Some("private query".to_string());
        search.search_limit = Some(7);
        search.fetch_page_content = Some(true);
        let context = Fetcher::product_billing_context(&search, ProductRoute::Search).unwrap();

        assert_eq!(context["query"], "private query");
        assert_eq!(context["route"], "search");
        assert_eq!(context["upstream_parameters"]["search_limit"], 7);
        assert_eq!(context["upstream_parameters"]["fetch_page_content"], true);
        assert_eq!(
            Fetcher::receipt_billing_context("opaque-receipt")["receipt_id"],
            "opaque-receipt"
        );
    }
}
