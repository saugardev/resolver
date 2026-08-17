//! Spider execution layer for product routes and receipts.

use crate::auth::ResolverAuthContext;
use crate::errors::FetchError;
use crate::provenance::{ProvenanceClient, ResolverFetchEvidence};
use crate::snapshot_upload::SnapshotPayload;
use crate::types::{
    FetchWithReceipt, FormatSelection, ProductFormat, ProductMode, ProductProxy, ProductRequest,
    ProductResponse, ProductRoute, Receipt,
};
use axum::http::header;
use reqwest::{Client, Response};
use serde::Serialize;
use serde_json::Value;
use spider_client::{
    CSSSelector, Delay, Engine, IdleNetwork, ProxyType, RequestParams, RequestType, ReturnFormat,
    ReturnFormatHandling, SearchRequestParams, Selector, Timeout, WaitFor,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

const DEFAULT_SPIDER_API_URL: &str = "https://api.spider.cloud";
const DEFAULT_MAX_UPSTREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_UPSTREAM_ERROR_BYTES: usize = 4 * 1024;
const SPIDER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SPIDER_CLIENT_TIMEOUT: Duration = Duration::from_secs(65);

/// Resolver-owned Spider transport.
///
/// The upstream crate's high-level helpers parse the body before exposing the
/// HTTP status and buffer the entire response. Keeping this boundary here lets
/// every operation enforce the same status, deadline, and response-size policy.
struct SpiderTransport {
    client: Client,
    api_key: String,
    api_url: String,
    max_response_bytes: usize,
}

impl SpiderTransport {
    fn from_env(api_key: String) -> Result<Self, String> {
        let api_url =
            std::env::var("SPIDER_API_URL").unwrap_or_else(|_| DEFAULT_SPIDER_API_URL.to_string());
        let max_response_bytes = match std::env::var("LIVY_RESOLVER_MAX_UPSTREAM_BYTES") {
            Ok(value) => value
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    "LIVY_RESOLVER_MAX_UPSTREAM_BYTES must be a positive integer".to_string()
                })?,
            Err(std::env::VarError::NotPresent) => DEFAULT_MAX_UPSTREAM_BYTES,
            Err(err) => {
                return Err(format!(
                    "cannot read LIVY_RESOLVER_MAX_UPSTREAM_BYTES: {err}"
                ));
            }
        };
        let client = Client::builder()
            .connect_timeout(SPIDER_CONNECT_TIMEOUT)
            .timeout(SPIDER_CLIENT_TIMEOUT)
            .build()
            .map_err(|err| format!("cannot configure Spider HTTP client: {err}"))?;

        Self::new(client, api_key, api_url, max_response_bytes)
    }

    fn new(
        client: Client,
        api_key: String,
        api_url: String,
        max_response_bytes: usize,
    ) -> Result<Self, String> {
        if max_response_bytes == 0 {
            return Err("Spider response limit must be greater than zero".to_string());
        }
        let api_url = api_url.trim_end_matches('/').to_string();
        let parsed = reqwest::Url::parse(&api_url)
            .map_err(|_| "SPIDER_API_URL must be a valid absolute URL".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("SPIDER_API_URL must use http or https".to_string());
        }

        Ok(Self {
            client,
            api_key,
            api_url,
            max_response_bytes,
        })
    }

    async fn post<T: Serialize + ?Sized>(
        &self,
        endpoint: &str,
        payload: &T,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        tokio::time::timeout_at(deadline, self.post_inner(endpoint, payload))
            .await
            .map_err(|_| FetchError::Timeout("Spider request deadline elapsed".to_string()))?
    }

    async fn post_inner<T: Serialize + ?Sized>(
        &self,
        endpoint: &str,
        payload: &T,
    ) -> Result<Value, FetchError> {
        let url = format!("{}/{endpoint}", self.api_url);
        let response = self
            .client
            .post(url)
            .header(
                header::USER_AGENT,
                concat!("Livy-Resolver/", env!("CARGO_PKG_VERSION")),
            )
            .header(header::ACCEPT, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.api_key))
            .json(payload)
            .send()
            .await
            .map_err(FetchError::UnableFetch)?;
        let status = response.status();

        if !status.is_success() {
            let body = read_error_body(response).await;
            return Err(FetchError::UpstreamStatus { status, body });
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(FetchError::UpstreamResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }

        let body = read_success_body(response, self.max_response_bytes).await?;
        parse_spider_body(&content_type, &body)
    }
}

async fn read_success_body(mut response: Response, limit: usize) -> Result<Vec<u8>, FetchError> {
    let mut body =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(limit as u64) as usize);
    while let Some(chunk) = response.chunk().await.map_err(FetchError::UnableFetch)? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .filter(|length| *length <= limit)
            .ok_or(FetchError::UpstreamResponseTooLarge { limit })?;
        body.reserve(next_len.saturating_sub(body.capacity()));
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_error_body(mut response: Response) -> String {
    let mut body = Vec::new();
    while body.len() < MAX_UPSTREAM_ERROR_BYTES {
        let Ok(Some(chunk)) = response.chunk().await else {
            break;
        };
        let remaining = MAX_UPSTREAM_ERROR_BYTES - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    String::from_utf8_lossy(&body).trim().to_string()
}

fn parse_spider_body(content_type: &str, body: &[u8]) -> Result<Value, FetchError> {
    if content_type.contains("jsonl") || content_type.contains("ndjson") {
        let text = String::from_utf8_lossy(body);
        let mut values = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            values.push(
                serde_json::from_str(line).map_err(|_| {
                    FetchError::Upstream("Spider returned invalid JSONL".to_string())
                })?,
            );
        }
        return Ok(Value::Array(values));
    }
    if content_type.contains("json") {
        return serde_json::from_slice(body)
            .map_err(|_| FetchError::Upstream("Spider returned invalid JSON".to_string()));
    }

    Ok(Value::String(String::from_utf8_lossy(body).into_owned()))
}

pub struct Fetcher {
    spider: SpiderTransport,
    provenance: Option<ProvenanceClient>,
    receipts: Mutex<HashMap<String, Receipt>>,
    receipt_counter: AtomicU64,
}

impl Fetcher {
    pub fn new() -> Self {
        dotenvy::dotenv().ok();
        let key = std::env::var("LIVY_RESOLVER_KEY")
            .or_else(|_| std::env::var("SPIDER_API_KEY"))
            .or_else(|_| std::env::var("SPIDER_KEY"))
            .or_else(|_| std::env::var("LIVY_KEY"))
            .expect("LIVY_RESOLVER_KEY must be set");
        let spider = SpiderTransport::from_env(key)
            .unwrap_or_else(|err| panic!("invalid Spider transport configuration: {err}"));
        let provenance = ProvenanceClient::from_env()
            .unwrap_or_else(|err| panic!("invalid provenance configuration: {err}"));
        Fetcher {
            spider,
            provenance,
            receipts: Mutex::new(HashMap::new()),
            receipt_counter: AtomicU64::new(1),
        }
    }

    pub async fn product_fetch(
        &self,
        payload: ProductRequest,
        route: ProductRoute,
    ) -> Result<ProductResponse, FetchError> {
        self.product_fetch_with_auth(payload, route, None).await
    }

    pub async fn product_fetch_with_auth(
        &self,
        payload: ProductRequest,
        route: ProductRoute,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<ProductResponse, FetchError> {
        payload.validate_for(route)?;
        let mode = self.resolve_mode(payload.mode, route);
        let timeout_secs = payload.timeout_secs.unwrap_or(match mode {
            ProductMode::Unblock => 50,
            ProductMode::Browser | ProductMode::Crawl | ProductMode::Screenshot => 45,
            _ => 25,
        });
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);

        let (data, executed_mode) = match route {
            ProductRoute::Search => {
                let query = payload
                    .query
                    .as_deref()
                    .ok_or_else(|| FetchError::BadRequest("search requires `query`".to_string()))?;
                (self.search(query, &payload, deadline).await?, mode)
            }
            ProductRoute::Map => {
                let source = Self::source(&payload)?;
                (self.map(source, &payload, deadline).await?, mode)
            }
            ProductRoute::Crawl => {
                let source = Self::source(&payload)?;
                (self.crawl(source, &payload, deadline).await?, mode)
            }
            ProductRoute::Screenshot => {
                let source = Self::source(&payload)?;
                (self.screenshot(source, &payload, deadline).await?, mode)
            }
            ProductRoute::Unblock => {
                let source = Self::source(&payload)?;
                (self.unblock(source, &payload, deadline).await?, mode)
            }
            ProductRoute::Extract | ProductRoute::Scrape => {
                let source = Self::source(&payload)?;
                if mode == ProductMode::Unblock {
                    (self.unblock(source, &payload, deadline).await?, mode)
                } else if matches!(mode, ProductMode::Auto | ProductMode::Fast) {
                    self.scrape_with_adaptive_fallback(source, &payload, mode, deadline)
                        .await?
                } else {
                    (self.scrape(source, &payload, mode, deadline).await?, mode)
                }
            }
        };

        let data = Self::ensure_spider_success(data)?;
        let should_receipt = payload.receipt.unwrap_or(matches!(
            mode,
            ProductMode::Auto | ProductMode::Fast | ProductMode::Extract
        ));
        let (receipt_id, receipt) = if should_receipt {
            let source = payload
                .source
                .as_deref()
                .or(payload.query.as_deref())
                .unwrap_or("unknown");
            let receipt = self.store_receipt(
                source,
                &data,
                executed_mode.receipt_label(),
                Self::request_type_name(executed_mode),
                Self::proxy_name(&payload, executed_mode),
            );
            (Some(receipt.id.clone()), Some(receipt))
        } else {
            (None, None)
        };

        let (provenance, provenance_error) = self
            .provenance_for(
                &payload,
                route,
                executed_mode,
                &data,
                receipt.as_ref(),
                auth_context,
            )
            .await;

        Ok(ProductResponse {
            route: route.as_str().to_string(),
            mode: executed_mode,
            receipt_id,
            receipt,
            data,
            provenance,
            provenance_error,
        })
    }

    pub async fn get_fast_data_with_receipt(
        &self,
        source: &str,
    ) -> Result<FetchWithReceipt, FetchError> {
        self.get_fast_data_with_receipt_with_auth(source, None)
            .await
    }

    pub async fn get_fast_data_with_receipt_with_auth(
        &self,
        source: &str,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<FetchWithReceipt, FetchError> {
        self.get_adaptive_data_with_receipt_with_auth(source, auth_context)
            .await
    }

    /// Try the fast path and perform one in-deadline unblock fallback before
    /// creating the final receipt or provenance record.
    pub async fn get_adaptive_data_with_receipt_with_auth(
        &self,
        source: &str,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<FetchWithReceipt, FetchError> {
        let payload = ProductRequest::fast(source);
        let response = self
            .product_fetch_with_auth(payload, ProductRoute::Scrape, auth_context)
            .await?;
        let crawl = response.data;
        let receipt = response
            .receipt
            .ok_or_else(|| FetchError::Http("receipt was not created".to_string()))?;

        Ok(FetchWithReceipt {
            receipt_id: receipt.id.clone(),
            receipt: receipt.clone(),
            data: crawl,
            provenance: response.provenance,
            provenance_error: response.provenance_error,
        })
    }

    pub async fn get_unblock_data_with_receipt_with_auth(
        &self,
        source: &str,
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<FetchWithReceipt, FetchError> {
        let mut payload = ProductRequest::fast(source);
        payload.mode = ProductMode::Unblock;
        payload.receipt = Some(true);
        let response = self
            .product_fetch_with_auth(payload, ProductRoute::Unblock, auth_context)
            .await?;
        let data = response.data;
        let receipt = response
            .receipt
            .ok_or_else(|| FetchError::Http("receipt was not created".to_string()))?;

        Ok(FetchWithReceipt {
            receipt_id: receipt.id.clone(),
            receipt: receipt.clone(),
            data,
            provenance: response.provenance,
            provenance_error: response.provenance_error,
        })
    }

    pub async fn snapshot_with_receipt(&self, source: &str) -> Result<SnapshotPayload, FetchError> {
        let formats = HashSet::from([ReturnFormat::Raw, ReturnFormat::Screenshot]);
        let params = RequestParams {
            return_format: Some(ReturnFormatHandling::Multi(formats)),
            request: Some(RequestType::SmartMode),
            stealth: Some(true),
            fingerprint: Some(true),
            proxy: Some(ProxyType::Isp),
            proxy_enabled: None,
            ..Default::default()
        };

        let request = Self::request_with_url(params, source)?;
        let deadline = Instant::now() + Duration::from_secs(50);
        let data = self.spider.post("scrape", &request, deadline).await?;
        let crawl = Self::ensure_spider_success(Self::normalize_value(data)?)?;

        SnapshotPayload::from_spider_response(source, crawl).map_err(FetchError::Snapshot)
    }

    pub async fn unblocker(&self, source: &str) -> Result<serde_json::Value, FetchError> {
        let mut payload = ProductRequest::fast(source);
        payload.mode = ProductMode::Unblock;
        payload.timeout_secs = Some(50);
        payload.request_timeout_secs = Some(30);
        payload.crawl_timeout_secs = Some(45);
        payload.stealth = Some(true);
        payload.fingerprint = Some(true);
        payload.scroll = Some(1);
        payload.receipt = Some(false);
        self.product_fetch(payload, ProductRoute::Unblock)
            .await
            .map(|response| response.data)
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
        mode: ProductMode,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        let params = self.request_params(payload, mode);
        let request = Self::request_with_url(params, source)?;
        Self::normalize_value(self.spider.post("scrape", &request, deadline).await?)
    }

    async fn scrape_with_adaptive_fallback(
        &self,
        source: &str,
        payload: &ProductRequest,
        mode: ProductMode,
        deadline: Instant,
    ) -> Result<(Value, ProductMode), FetchError> {
        let first = self
            .scrape(source, payload, mode, deadline)
            .await
            .and_then(Self::ensure_spider_success);

        let fallback_reason = match &first {
            Ok(data) => fallback_reason_for_fetch_data(data),
            Err(error) => fallback_reason_for_fetch_error(error),
        };
        let Some(reason) = fallback_reason else {
            return first.map(|data| (data, mode));
        };

        eprintln!(
            "{}",
            serde_json::json!({
                "event": "resolver_fetch_adaptive_fallback",
                "request_id": crate::security::current_request_id(),
                "reason": reason,
                "source_sha256": crate::security::sensitive_hash(source),
            })
        );
        let data = self
            .unblock(source, payload, deadline)
            .await
            .and_then(Self::ensure_spider_success)?;
        Ok((data, ProductMode::Unblock))
    }

    async fn unblock(
        &self,
        source: &str,
        payload: &ProductRequest,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        let params = self.request_params(payload, ProductMode::Unblock);
        let request = Self::request_with_url(params, source)?;
        Self::normalize_value(self.spider.post("unblocker", &request, deadline).await?)
    }

    async fn crawl(
        &self,
        source: &str,
        payload: &ProductRequest,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        let params = self.request_params(payload, ProductMode::Crawl);
        let request = Self::request_with_url(params, source)?;
        Self::normalize_value(self.spider.post("crawl", &request, deadline).await?)
    }

    async fn map(
        &self,
        source: &str,
        payload: &ProductRequest,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        let params = self.request_params(payload, ProductMode::Map);
        let request = Self::request_with_url(params, source)?;
        Self::normalize_value(self.spider.post("links", &request, deadline).await?)
    }

    async fn screenshot(
        &self,
        source: &str,
        payload: &ProductRequest,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        let mut params = self.request_params(payload, ProductMode::Screenshot);
        params.return_format = Some(ReturnFormatHandling::Single(ReturnFormat::Screenshot));
        let request = Self::request_with_url(params, source)?;
        Self::normalize_value(self.spider.post("screenshot", &request, deadline).await?)
    }

    async fn search(
        &self,
        query: &str,
        payload: &ProductRequest,
        deadline: Instant,
    ) -> Result<Value, FetchError> {
        let base = self.request_params(payload, ProductMode::Search);
        let params = SearchRequestParams {
            base,
            search: query.to_string(),
            search_limit: payload.search_limit.or(payload.limit),
            fetch_page_content: payload.fetch_page_content,
            num: payload.limit,
            quick_search: payload.quick_search,
            engine: payload.engine.as_deref().and_then(parse_engine),
            ..Default::default()
        };
        let request = Self::request_value(params)?;
        Self::normalize_value(self.spider.post("search", &request, deadline).await?)
    }

    /// Resolve the route into the mode that should be executed.
    fn resolve_mode(&self, mode: ProductMode, route: ProductRoute) -> ProductMode {
        match route {
            ProductRoute::Crawl => ProductMode::Crawl,
            ProductRoute::Map => ProductMode::Map,
            ProductRoute::Search => ProductMode::Search,
            ProductRoute::Extract => ProductMode::Extract,
            ProductRoute::Screenshot => ProductMode::Screenshot,
            ProductRoute::Unblock => ProductMode::Unblock,
            ProductRoute::Scrape => mode,
        }
    }

    /// Return the required source URL from a product request.
    fn source(payload: &ProductRequest) -> Result<&str, FetchError> {
        payload.require_source()
    }

    fn request_with_url(params: RequestParams, source: &str) -> Result<Value, FetchError> {
        let mut request = Self::request_value(params)?;
        let object = request.as_object_mut().ok_or_else(|| {
            FetchError::Http("Spider request parameters did not serialize as an object".to_string())
        })?;
        object.insert("url".to_string(), Value::String(source.to_string()));
        Ok(request)
    }

    fn request_value<T: Serialize>(params: T) -> Result<Value, FetchError> {
        let mut request = serde_json::to_value(params)?;
        let object = request.as_object_mut().ok_or_else(|| {
            FetchError::Http("Spider request parameters did not serialize as an object".to_string())
        })?;
        object.retain(|_, value| !value.is_null());
        Ok(request)
    }

    /// Build Spider request params from product options.
    fn request_params(&self, payload: &ProductRequest, mode: ProductMode) -> RequestParams {
        RequestParams {
            request: Some(Self::request_type(mode)),
            return_format: Some(Self::return_format(payload, mode)),
            proxy: Self::proxy_type(payload, mode),
            // `proxy_enabled` is deprecated by Spider. Sending it together with
            // `proxy` creates contradictory requests (for example ISP + false).
            proxy_enabled: None,
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
            Value::String(s) => serde_json::from_str::<Value>(&s)
                .map_err(|_| FetchError::Upstream("Spider returned invalid JSON".to_string())),
            other => Ok(other),
        }
    }

    fn ensure_spider_success(value: Value) -> Result<Value, FetchError> {
        let values: Box<dyn Iterator<Item = &Value> + '_> = match value.as_array() {
            Some(items) => Box::new(items.iter()),
            None => Box::new(std::iter::once(&value)),
        };

        for item in values {
            let status = item.get("status").and_then(Value::as_i64);
            let success = item.get("success").and_then(Value::as_bool);
            let error = item.get("error").and_then(|error| match error {
                Value::Null => None,
                Value::Bool(false) => None,
                Value::String(message) if message.trim().is_empty() => None,
                Value::Array(items) if items.is_empty() => None,
                Value::Object(fields) if fields.is_empty() => None,
                Value::String(message) => Some(message.clone()),
                other => Some(other.to_string()),
            });

            if matches!(status, Some(0) | Some(400..=599))
                || matches!(success, Some(false))
                || error.is_some()
            {
                return Err(FetchError::Upstream(error.unwrap_or_else(|| {
                    "Spider returned an unsuccessful fetch status".to_string()
                })));
            }
        }

        Ok(value)
    }

    pub fn get_receipt(&self, id: &str) -> Option<Receipt> {
        self.receipts
            .lock()
            .ok()
            .and_then(|receipts| receipts.get(id).cloned())
    }

    fn store_receipt(
        &self,
        source: &str,
        value: &Value,
        mode: &str,
        request_type: &str,
        proxy: Option<&str>,
    ) -> Receipt {
        let id = self.next_receipt_id();
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

        if let Ok(mut receipts) = self.receipts.lock() {
            receipts.insert(receipt.id.clone(), receipt.clone());
        }

        receipt
    }

    fn next_receipt_id(&self) -> String {
        let sequence = self.receipt_counter.fetch_add(1, Ordering::Relaxed);
        format!("{:x}-{:x}", Self::now_unix_ms(), sequence)
    }

    fn now_unix_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    }
}

pub(crate) fn fallback_reason_for_fetch_error(error: &FetchError) -> Option<&'static str> {
    match error {
        FetchError::Upstream(detail) => fallback_reason_for_text(detail),
        FetchError::UpstreamStatus { body, .. } => fallback_reason_for_text(body),
        _ => None,
    }
}

pub(crate) fn fallback_reason_for_fetch_data(data: &Value) -> Option<&'static str> {
    if let Some(content) = extracted_content(data) {
        return fallback_reason_for_text(content);
    }
    fallback_reason_for_text(&data.to_string())
}

fn extracted_content(data: &Value) -> Option<&str> {
    if let Some(content) = data.get("content").and_then(Value::as_str) {
        return Some(content);
    }

    data.as_array()?
        .iter()
        .find_map(|item| item.get("content").and_then(Value::as_str))
}

pub(crate) fn fallback_reason_for_text(text: &str) -> Option<&'static str> {
    let normalized = text
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if contains_any(
        &normalized,
        &[
            "enable javascript",
            "requires javascript",
            "require javascript",
            "javascript is disabled",
            "javascript disabled",
            "please enable js",
            "turn on javascript",
            "you need javascript",
            "browser is required",
        ],
    ) {
        return Some("javascript_required");
    }
    if contains_any(
        &normalized,
        &[
            "blocked by robots",
            "disallowed by robots",
            "robots.txt",
            "robots policy",
            "respect robots",
        ],
    ) {
        return Some("robots_blocked");
    }
    if contains_any(
        &normalized,
        &[
            "verify you are human",
            "verify that you are human",
            "confirm you are human",
            "prove you are human",
            "are you a human",
            "human verification",
            "not a robot",
            "are not a robot",
            "verify you are not a robot",
            "complete the security check",
            "security check to access",
            "captcha",
        ],
    ) {
        return Some("human_verification");
    }
    if contains_any(
        &normalized,
        &[
            "checking your browser",
            "checking if the site connection is secure",
            "just a moment...",
        ],
    ) || (normalized.contains("cloudflare")
        && contains_any(
            &normalized,
            &["ray id", "attention required", "challenge", "turnstile"],
        ))
    {
        return Some("browser_check");
    }

    None
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
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
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        http::{Response as HttpResponse, StatusCode},
        routing::post,
    };
    use serde_json::json;

    async fn spawn_mock(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock Spider");
        let address = listener.local_addr().expect("mock address");
        tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve mock Spider");
        });
        format!("http://{address}")
    }

    fn test_fetcher(api_url: String, max_response_bytes: usize) -> Fetcher {
        let transport = SpiderTransport::new(
            Client::builder().build().expect("test HTTP client"),
            "test-key".to_string(),
            api_url,
            max_response_bytes,
        )
        .expect("test Spider transport");
        Fetcher {
            spider: transport,
            provenance: None,
            receipts: Mutex::new(HashMap::new()),
            receipt_counter: AtomicU64::new(1),
        }
    }

    #[tokio::test]
    async fn every_operation_rejects_http_errors_before_receipt_creation() {
        let router = Router::new().fallback(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": "upstream unavailable"})),
            )
        });
        let fetcher = test_fetcher(spawn_mock(router).await, 1024);

        for route in [
            ProductRoute::Scrape,
            ProductRoute::Crawl,
            ProductRoute::Map,
            ProductRoute::Search,
            ProductRoute::Extract,
            ProductRoute::Screenshot,
            ProductRoute::Unblock,
        ] {
            let mut request = ProductRequest::legacy("https://example.com");
            request.receipt = Some(true);
            if route == ProductRoute::Search {
                request.query = Some("resolver test".to_string());
            }

            let error = fetcher
                .product_fetch(request, route)
                .await
                .err()
                .expect("HTTP 500 must fail");
            assert!(matches!(
                error,
                FetchError::UpstreamStatus { status, .. }
                    if status == StatusCode::INTERNAL_SERVER_ERROR
            ));
        }

        let snapshot_error = fetcher
            .snapshot_with_receipt("https://example.com")
            .await
            .expect_err("snapshot HTTP 500 must fail");
        assert!(matches!(
            snapshot_error,
            FetchError::UpstreamStatus { status, .. }
                if status == StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(fetcher.receipts.lock().expect("receipts").is_empty());
    }

    #[tokio::test]
    async fn response_body_limit_is_enforced_before_json_parsing() {
        let router = Router::new().route(
            "/scrape",
            post(|| async {
                HttpResponse::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("x".repeat(65)))
                    .expect("large response")
            }),
        );
        let fetcher = test_fetcher(spawn_mock(router).await, 64);
        let mut request = ProductRequest::legacy("https://example.com");
        request.receipt = Some(true);

        let error = fetcher
            .product_fetch(request, ProductRoute::Scrape)
            .await
            .err()
            .expect("oversized response must fail");
        assert!(matches!(
            error,
            FetchError::UpstreamResponseTooLarge { limit: 64 }
        ));
        assert!(fetcher.receipts.lock().expect("receipts").is_empty());
    }

    #[tokio::test]
    async fn absolute_deadline_covers_request_and_body() {
        let router = Router::new().route(
            "/slow",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Json(json!({"status": 200}))
            }),
        );
        let transport = SpiderTransport::new(
            Client::new(),
            "test-key".to_string(),
            spawn_mock(router).await,
            1024,
        )
        .expect("test transport");

        let error = transport
            .post(
                "slow",
                &json!({}),
                Instant::now() + Duration::from_millis(20),
            )
            .await
            .expect_err("deadline must expire");
        assert!(matches!(error, FetchError::Timeout(_)));
    }

    #[tokio::test]
    async fn adaptive_fallback_retries_now_and_creates_only_final_receipt() {
        let router = Router::new()
            .route(
                "/scrape",
                post(|| async {
                    Json(json!([{
                        "status": 200,
                        "content": "Please enable JavaScript to continue"
                    }]))
                }),
            )
            .route(
                "/unblocker",
                post(|| async {
                    Json(json!([{
                        "status": 200,
                        "content": "resolved content"
                    }]))
                }),
            );
        let fetcher = test_fetcher(spawn_mock(router).await, 4096);
        let request = ProductRequest::fast("https://example.com");

        let response = fetcher
            .product_fetch(request, ProductRoute::Scrape)
            .await
            .expect("adaptive request succeeds");

        assert_eq!(response.mode, ProductMode::Unblock);
        assert_eq!(response.data[0]["content"], "resolved content");
        assert_eq!(
            response.receipt.as_ref().expect("receipt").mode,
            "fetch:unblock"
        );
        assert_eq!(fetcher.receipts.lock().expect("receipts").len(), 1);
    }

    #[tokio::test]
    async fn structured_http_error_can_trigger_one_safe_current_request_fallback() {
        let router = Router::new()
            .route(
                "/scrape",
                post(|| async { (StatusCode::FORBIDDEN, "captcha verification required") }),
            )
            .route(
                "/unblocker",
                post(|| async { Json(json!([{"status": 200, "content": "resolved"}])) }),
            );
        let fetcher = test_fetcher(spawn_mock(router).await, 4096);

        let response = fetcher
            .product_fetch(
                ProductRequest::fast("https://example.com"),
                ProductRoute::Scrape,
            )
            .await
            .expect("classified challenge is rescued");
        assert_eq!(response.mode, ProductMode::Unblock);
        assert_eq!(fetcher.receipts.lock().expect("receipts").len(), 1);
    }

    #[test]
    fn proxy_contract_uses_only_the_current_spider_field() {
        let fetcher = test_fetcher("http://127.0.0.1:1".to_string(), 1024);
        let request = ProductRequest::fast("https://example.com");
        let params = fetcher.request_params(&request, ProductMode::Fast);
        assert!(params.proxy_enabled.is_none());
        assert_eq!(params.proxy, Some(ProxyType::Isp));

        let wire = Fetcher::request_with_url(params, "https://example.com").expect("wire request");
        assert_eq!(wire["proxy"], "isp");
        assert!(wire.get("proxy_enabled").is_none());
    }

    #[test]
    fn failure_in_any_spider_item_rejects_the_whole_operation() {
        let error = Fetcher::ensure_spider_success(json!([
            {"status": 200, "content": "ok"},
            {"status": 0, "error": "second page failed"}
        ]))
        .expect_err("partial failure must not look successful");

        assert!(matches!(error, FetchError::Upstream(_)));
    }
}
