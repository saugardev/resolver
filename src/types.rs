//! Product-facing request, route, mode, and receipt types.

use crate::provenance::ProvenanceResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use url::Url;

const MAX_URL_LEN: usize = 8_192;
const MAX_QUERY_LEN: usize = 2_048;
const MAX_LIST_ENTRIES: usize = 100;
const MAX_STRING_LEN: usize = 2_048;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// High-level route behavior exposed to API clients.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProductMode {
    /// Let the service choose the default SmartMode path.
    #[serde(alias = "smart", alias = "smart_mode")]
    #[default]
    Auto,
    /// Use the fast SmartMode + ISP proxy source-fetch path.
    Fast,
    /// Use browser rendering for JavaScript-heavy pages.
    Browser,
    /// Use browser rendering with stealth and proxy defaults.
    Unblock,
    /// Use a plain HTTP request without readability cleanup.
    Raw,
    /// Crawl a website with depth and limit controls.
    Crawl,
    /// Return discovered links without fetching every page.
    Map,
    /// Search the web and optionally fetch result pages.
    Search,
    /// Fetch content prepared for downstream structured extraction.
    Extract,
    /// Return a page screenshot payload.
    Screenshot,
}

impl ProductMode {
    /// Stable receipt label for the selected mode.
    pub fn receipt_label(self) -> &'static str {
        match self {
            Self::Auto => "fetch:auto",
            Self::Fast => "fetch:fast",
            Self::Browser => "fetch:browser",
            Self::Unblock => "fetch:unblock",
            Self::Raw => "fetch:raw",
            Self::Crawl => "crawl",
            Self::Map => "map",
            Self::Search => "search",
            Self::Extract => "extract",
            Self::Screenshot => "screenshot",
        }
    }
}

/// Output formats clients can request from Spider.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProductFormat {
    /// Return raw source content.
    Raw,
    /// Return markdown content.
    Markdown,
    /// Return CommonMark content.
    Commonmark,
    /// Return html2text content.
    Html2text,
    /// Return plain text content.
    Text,
    /// Return a screenshot payload.
    Screenshot,
    /// Return XML content.
    Xml,
    /// Return bytes content.
    Bytes,
}

/// Single or multiple output format selection.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum FormatSelection {
    /// One requested format.
    One(ProductFormat),
    /// Multiple requested formats.
    Many(Vec<ProductFormat>),
}

/// Proxy policy requested by the caller.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProductProxy {
    /// Let the route choose a proxy default.
    Auto,
    /// Do not request a proxy.
    None,
    /// Use ISP proxy routing.
    Isp,
    /// Use residential proxy routing.
    Residential,
    /// Use mobile proxy routing.
    Mobile,
}

/// Product API request accepted by route handlers.
#[derive(Clone, Debug, Deserialize)]
pub struct ProductRequest {
    /// Exact URL to fetch, crawl, map, extract, or screenshot.
    #[serde(alias = "url")]
    pub source: Option<String>,
    /// Search query for the search route.
    #[serde(alias = "q")]
    pub query: Option<String>,
    /// High-level execution mode.
    #[serde(default)]
    pub mode: ProductMode,
    /// Output format as one value or an array.
    pub format: Option<FormatSelection>,
    /// Output formats as an explicit array.
    pub formats: Option<Vec<ProductFormat>>,
    /// Proxy routing preference.
    pub proxy: Option<ProductProxy>,
    /// Maximum page or result count.
    pub limit: Option<u32>,
    /// Crawl depth.
    pub depth: Option<u32>,
    /// Outer service timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Per-request upstream timeout in seconds.
    pub request_timeout_secs: Option<u8>,
    /// Upstream crawl timeout in seconds.
    pub crawl_timeout_secs: Option<u64>,
    /// Enable readability cleanup.
    pub readability: Option<bool>,
    /// Allow upstream caching.
    pub cache: Option<bool>,
    /// Return metadata in upstream payloads.
    pub metadata: Option<bool>,
    /// Return response headers.
    pub return_headers: Option<bool>,
    /// Return discovered page links.
    pub return_page_links: Option<bool>,
    /// Return response cookies.
    pub return_cookies: Option<bool>,
    /// Country code for request routing.
    pub country_code: Option<String>,
    /// Locale for browser or request hints.
    pub locale: Option<String>,
    /// Custom user agent.
    pub user_agent: Option<String>,
    /// Custom request headers.
    pub headers: Option<HashMap<String, String>>,
    /// Request cookies.
    pub cookies: Option<String>,
    /// Root CSS selector for content filtering.
    pub root_selector: Option<String>,
    /// Named CSS selectors for extraction.
    pub selectors: Option<HashMap<String, Vec<String>>>,
    /// URL allow-list patterns.
    pub whitelist: Option<Vec<String>>,
    /// URL block-list patterns.
    pub blacklist: Option<Vec<String>>,
    /// Restrict crawling to top-level domain.
    pub tld: Option<bool>,
    /// Include subdomains while crawling.
    pub subdomains: Option<bool>,
    /// External domains allowed during crawl.
    pub external_domains: Option<Vec<String>>,
    /// Use sitemap discovery.
    pub sitemap: Option<bool>,
    /// Respect robots.txt.
    pub respect_robots: Option<bool>,
    /// Enable stealth browser behavior.
    pub stealth: Option<bool>,
    /// Enable browser fingerprint handling.
    pub fingerprint: Option<bool>,
    /// Number of scroll steps.
    pub scroll: Option<u32>,
    /// Hard wait in milliseconds.
    pub wait_ms: Option<u64>,
    /// Selector to wait for.
    pub wait_selector: Option<String>,
    /// Disable request interception.
    pub disable_intercept: Option<bool>,
    /// Disable upstream optimization hints.
    pub disable_hints: Option<bool>,
    /// Use lower-cost lite mode.
    pub lite_mode: Option<bool>,
    /// Maximum upstream credits per page.
    pub max_credits_per_page: Option<f64>,
    /// Whether to create an in-memory receipt.
    pub receipt: Option<bool>,
    /// Number of search results to request.
    pub search_limit: Option<u32>,
    /// Fetch content for search results.
    pub fetch_page_content: Option<bool>,
    /// Prefer faster search results.
    pub quick_search: Option<bool>,
    /// Search engine selector.
    pub engine: Option<String>,
}

impl ProductRequest {
    /// Validate route-specific and bounded input before charging or calling Spider.
    pub fn validate_for(&self, route: ProductRoute) -> Result<(), crate::errors::FetchError> {
        use crate::errors::FetchError;

        if route == ProductRoute::Search {
            validate_required_string("query", self.query.as_deref(), MAX_QUERY_LEN)?;
        } else {
            validate_source_url(self.require_source()?)?;
        }

        validate_range("limit", self.limit.map(u64::from), 1, 100)?;
        validate_range("search_limit", self.search_limit.map(u64::from), 1, 100)?;
        validate_range("depth", self.depth.map(u64::from), 0, 10)?;
        validate_range("timeout_secs", self.timeout_secs, 1, 60)?;
        validate_range(
            "request_timeout_secs",
            self.request_timeout_secs.map(u64::from),
            1,
            60,
        )?;
        validate_range("crawl_timeout_secs", self.crawl_timeout_secs, 1, 60)?;
        validate_range("wait_ms", self.wait_ms, 0, 30_000)?;
        validate_range("scroll", self.scroll.map(u64::from), 0, 100)?;

        if let Some(formats) = &self.formats {
            validate_collection_len("formats", formats.len(), 1, 8)?;
        }
        if let Some(FormatSelection::Many(formats)) = &self.format {
            validate_collection_len("format", formats.len(), 1, 8)?;
        }
        if let Some(country) = &self.country_code
            && (country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_alphabetic()))
        {
            return Err(FetchError::BadRequest(
                "`country_code` must contain two ASCII letters".into(),
            ));
        }
        validate_optional_string("locale", self.locale.as_deref(), 64)?;
        validate_optional_string("user_agent", self.user_agent.as_deref(), 512)?;
        validate_optional_string("cookies", self.cookies.as_deref(), MAX_HEADER_BYTES)?;
        validate_optional_string("root_selector", self.root_selector.as_deref(), 1_024)?;
        validate_optional_string("wait_selector", self.wait_selector.as_deref(), 1_024)?;
        validate_string_list("whitelist", self.whitelist.as_deref())?;
        validate_string_list("blacklist", self.blacklist.as_deref())?;
        validate_string_list("external_domains", self.external_domains.as_deref())?;
        validate_headers(self.headers.as_ref())?;
        validate_selectors(self.selectors.as_ref())?;

        if let Some(engine) = self.engine.as_deref()
            && !matches!(
                engine.to_ascii_lowercase().as_str(),
                "google" | "brave" | "all"
            )
        {
            return Err(FetchError::BadRequest(
                "`engine` must be one of: google, brave, all".into(),
            ));
        }
        if let Some(value) = self.max_credits_per_page
            && (!value.is_finite() || value <= 0.0)
        {
            return Err(FetchError::BadRequest(
                "`max_credits_per_page` must be finite and greater than zero".into(),
            ));
        }

        Ok(())
    }

    /// Legacy exact-source request used by older handlers.
    pub fn legacy(source: &str) -> Self {
        Self {
            source: Some(source.to_string()),
            query: None,
            mode: ProductMode::Auto,
            format: None,
            formats: None,
            proxy: Some(ProductProxy::None),
            limit: None,
            depth: None,
            timeout_secs: Some(25),
            request_timeout_secs: Some(20),
            crawl_timeout_secs: Some(25),
            readability: Some(true),
            cache: None,
            metadata: None,
            return_headers: None,
            return_page_links: None,
            return_cookies: None,
            country_code: None,
            locale: None,
            user_agent: None,
            headers: None,
            cookies: None,
            root_selector: None,
            selectors: None,
            whitelist: None,
            blacklist: None,
            tld: None,
            subdomains: None,
            external_domains: None,
            sitemap: None,
            respect_robots: None,
            stealth: None,
            fingerprint: None,
            scroll: None,
            wait_ms: None,
            wait_selector: None,
            disable_intercept: None,
            disable_hints: None,
            lite_mode: None,
            max_credits_per_page: None,
            receipt: Some(false),
            search_limit: None,
            fetch_page_content: None,
            quick_search: None,
            engine: None,
        }
    }

    /// Fast source request used by MCP and receipt-backed fetches.
    pub fn fast(source: &str) -> Self {
        Self {
            mode: ProductMode::Fast,
            proxy: Some(ProductProxy::Isp),
            receipt: Some(true),
            ..Self::legacy(source)
        }
    }

    /// Return the request URL or a clear API error.
    pub fn require_source(&self) -> Result<&str, crate::errors::FetchError> {
        self.source
            .as_deref()
            .ok_or_else(|| crate::errors::FetchError::BadRequest("route requires `source`".into()))
    }
}

pub fn validate_source_url(source: &str) -> Result<(), crate::errors::FetchError> {
    use crate::errors::FetchError;

    if source.is_empty() || source.len() > MAX_URL_LEN || source.trim() != source {
        return Err(FetchError::BadRequest(
            "`source` must be a non-empty URL no longer than 8192 characters".into(),
        ));
    }
    let parsed = Url::parse(source)
        .map_err(|_| FetchError::BadRequest("`source` must be a valid absolute URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(FetchError::BadRequest(
            "`source` must use the http or https scheme".into(),
        ));
    }
    if parsed.host().is_none() {
        return Err(FetchError::BadRequest(
            "`source` must include a host".into(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(FetchError::BadRequest(
            "`source` must not contain embedded credentials".into(),
        ));
    }
    Ok(())
}

pub fn validate_idempotency_key(value: Option<&str>) -> Result<(), crate::errors::FetchError> {
    if let Some(value) = value {
        let trimmed = value.trim();
        if trimmed.is_empty()
            || trimmed.len() > 128
            || !trimmed.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(crate::errors::FetchError::BadRequest(
                "`idempotency_key` must be 1-128 safe ASCII characters".into(),
            ));
        }
    }
    Ok(())
}

pub fn validate_receipt_id(value: &str) -> Result<(), crate::errors::FetchError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(crate::errors::FetchError::BadRequest(
            "invalid receipt id".into(),
        ));
    }
    Ok(())
}

fn validate_required_string(
    field: &str,
    value: Option<&str>,
    max: usize,
) -> Result<(), crate::errors::FetchError> {
    let value = value.ok_or_else(|| {
        crate::errors::FetchError::BadRequest(format!("route requires `{field}`"))
    })?;
    if value.trim().is_empty() || value.len() > max {
        return Err(crate::errors::FetchError::BadRequest(format!(
            "`{field}` must be non-empty and no longer than {max} characters"
        )));
    }
    Ok(())
}

fn validate_optional_string(
    field: &str,
    value: Option<&str>,
    max: usize,
) -> Result<(), crate::errors::FetchError> {
    if let Some(value) = value
        && value.len() > max
    {
        return Err(crate::errors::FetchError::BadRequest(format!(
            "`{field}` must be no longer than {max} characters"
        )));
    }
    Ok(())
}

fn validate_range(
    field: &str,
    value: Option<u64>,
    min: u64,
    max: u64,
) -> Result<(), crate::errors::FetchError> {
    if let Some(value) = value
        && !(min..=max).contains(&value)
    {
        return Err(crate::errors::FetchError::BadRequest(format!(
            "`{field}` must be between {min} and {max}"
        )));
    }
    Ok(())
}

fn validate_collection_len(
    field: &str,
    len: usize,
    min: usize,
    max: usize,
) -> Result<(), crate::errors::FetchError> {
    if !(min..=max).contains(&len) {
        return Err(crate::errors::FetchError::BadRequest(format!(
            "`{field}` must contain between {min} and {max} entries"
        )));
    }
    Ok(())
}

fn validate_string_list(
    field: &str,
    values: Option<&[String]>,
) -> Result<(), crate::errors::FetchError> {
    let Some(values) = values else { return Ok(()) };
    validate_collection_len(field, values.len(), 0, MAX_LIST_ENTRIES)?;
    if values.iter().any(|value| value.len() > MAX_STRING_LEN) {
        return Err(crate::errors::FetchError::BadRequest(format!(
            "`{field}` entries must be no longer than {MAX_STRING_LEN} characters"
        )));
    }
    Ok(())
}

fn validate_headers(
    headers: Option<&HashMap<String, String>>,
) -> Result<(), crate::errors::FetchError> {
    let Some(headers) = headers else {
        return Ok(());
    };
    validate_collection_len("headers", headers.len(), 0, MAX_HEADERS)?;
    let mut total = 0usize;
    for (name, value) in headers {
        total = total.saturating_add(name.len()).saturating_add(value.len());
        let parsed_name = name.parse::<axum::http::HeaderName>().map_err(|_| {
            crate::errors::FetchError::BadRequest(format!("invalid request header name `{name}`"))
        })?;
        value.parse::<axum::http::HeaderValue>().map_err(|_| {
            crate::errors::FetchError::BadRequest(format!(
                "invalid value for request header `{name}`"
            ))
        })?;
        if matches!(
            parsed_name.as_str(),
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "upgrade"
        ) {
            return Err(crate::errors::FetchError::BadRequest(format!(
                "request header `{name}` is not allowed"
            )));
        }
    }
    if total > MAX_HEADER_BYTES {
        return Err(crate::errors::FetchError::BadRequest(
            "request headers exceed 8192 bytes".into(),
        ));
    }
    Ok(())
}

fn validate_selectors(
    selectors: Option<&HashMap<String, Vec<String>>>,
) -> Result<(), crate::errors::FetchError> {
    let Some(selectors) = selectors else {
        return Ok(());
    };
    validate_collection_len("selectors", selectors.len(), 0, 32)?;
    for (name, values) in selectors {
        if name.is_empty() || name.len() > 128 {
            return Err(crate::errors::FetchError::BadRequest(
                "selector names must be 1-128 characters".into(),
            ));
        }
        validate_collection_len("selector values", values.len(), 1, 32)?;
        if values
            .iter()
            .any(|value| value.is_empty() || value.len() > 1_024)
        {
            return Err(crate::errors::FetchError::BadRequest(
                "selectors must be 1-1024 characters".into(),
            ));
        }
    }
    Ok(())
}

/// Internal route selected by each HTTP handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProductRoute {
    /// One-page scrape or fetch.
    Scrape,
    /// Website crawl.
    Crawl,
    /// Link discovery.
    Map,
    /// Web search.
    Search,
    /// Fetch for structured extraction.
    Extract,
    /// Screenshot capture.
    Screenshot,
    /// Stealth unblock fetch.
    Unblock,
}

impl ProductRoute {
    /// Public route name included in responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scrape => "fetch",
            Self::Crawl => "crawl",
            Self::Map => "map",
            Self::Search => "search",
            Self::Extract => "extract",
            Self::Screenshot => "screenshot",
            Self::Unblock => "unblock",
        }
    }
}

/// Resolver metadata for a tenant-owned receipt-backed request.
#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    /// Receipt identifier.
    pub id: String,
    /// Original URL or query.
    pub source_url: String,
    /// Product mode label.
    pub mode: String,
    /// Upstream request type label.
    pub request_type: String,
    /// Proxy label when one was used.
    pub proxy: Option<String>,
    /// Upstream status value.
    pub status: Option<i64>,
    /// Upstream error text.
    pub error: Option<String>,
    /// Upstream elapsed time in milliseconds.
    pub duration_elapsed_ms: Option<u64>,
    /// Extracted content byte length.
    pub content_bytes: Option<usize>,
    /// Upstream formatted total cost.
    pub total_cost: Option<String>,
    /// Receipt creation time.
    pub created_at_unix_ms: u128,
    /// Human-readable demo marker.
    pub demo_message: String,
}

/// Legacy receipt-backed fetch response.
#[derive(Serialize)]
pub struct FetchWithReceipt {
    /// Receipt identifier.
    pub receipt_id: String,
    /// Full receipt metadata.
    pub receipt: Receipt,
    /// Upstream payload.
    pub data: Value,
    /// Livy provenance record when provenance is enabled and succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceResult>,
    /// Livy provenance failure when fetching succeeded but attestation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_error: Option<String>,
}

/// Product route response envelope.
#[derive(Serialize)]
pub struct ProductResponse {
    /// Executed route name.
    pub route: String,
    /// Resolved product mode.
    pub mode: ProductMode,
    /// Receipt identifier when created.
    pub receipt_id: Option<String>,
    /// Full receipt metadata when created.
    pub receipt: Option<Receipt>,
    /// Upstream payload.
    pub data: Value,
    /// Livy provenance record when provenance is enabled and succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceResult>,
    /// Livy provenance failure when fetching succeeded but attestation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_validation_allows_http_private_destinations() {
        assert!(validate_source_url("http://127.0.0.1:8080/path").is_ok());
        assert!(validate_source_url("https://example.com/path").is_ok());
    }

    #[test]
    fn source_validation_rejects_credentials_and_non_web_schemes() {
        assert!(validate_source_url("https://user:pass@example.com").is_err());
        assert!(validate_source_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn product_validation_enforces_route_and_numeric_bounds() {
        let mut request = ProductRequest::legacy("https://example.com");
        request.limit = Some(101);
        assert!(request.validate_for(ProductRoute::Scrape).is_err());

        let search = ProductRequest {
            source: None,
            query: Some("rust axum".to_string()),
            ..ProductRequest::legacy("https://example.com")
        };
        assert!(search.validate_for(ProductRoute::Search).is_ok());
    }

    #[test]
    fn rejects_unsafe_forwarded_headers() {
        let mut request = ProductRequest::legacy("https://example.com");
        request.headers = Some(HashMap::from([(
            "Host".to_string(),
            "internal.example".to_string(),
        )]));
        assert!(request.validate_for(ProductRoute::Scrape).is_err());
    }

    #[test]
    fn validates_idempotency_and_receipt_identifiers() {
        assert!(validate_idempotency_key(Some("retry:request-1")).is_ok());
        assert!(validate_idempotency_key(Some("bad key")).is_err());
        assert!(validate_receipt_id("18f-2").is_ok());
        assert!(validate_receipt_id("../../secret").is_err());
    }
}
