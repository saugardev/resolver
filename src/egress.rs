//! Default-deny source egress checks.
//!
//! Spider performs the final fetch in separate infrastructure, so local DNS validation is only
//! defense in depth. Actual fetching remains disabled unless an operator explicitly attests that
//! the selected Spider deployment (or its policy proxy) enforces destination policy across DNS
//! resolution and redirects, and its bounded readiness probe succeeds.

use crate::{
    config::{env_bool, env_u64},
    errors::FetchError,
    types::validate_source_url,
};
use antissrf::{AntiSSRFPolicy, PolicyConfigOptions};
use serde::Deserialize;
use std::{collections::HashSet, net::IpAddr, time::Duration};
use tokio::net::lookup_host;
use url::{Host, Url};

const DEFAULT_DNS_TIMEOUT_SECS: u64 = 3;
const DEFAULT_CAPABILITY_TIMEOUT_SECS: u64 = 3;
const MAX_CAPABILITY_DOCUMENT_BYTES: usize = 4 * 1024;
pub const SPIDER_EGRESS_ATTESTATION: &str = "spider-egress-policy-v1";
pub const SPIDER_EGRESS_CAPABILITY_SCHEMA: &str = "livy.resolver.spider-egress-capability/v1";

#[derive(Clone)]
pub struct EgressPolicy {
    allow_private_sources: bool,
    trusted_hosts: HashSet<String>,
    dns_timeout: Duration,
    capability: Option<CapabilityProbe>,
    capability_client: reqwest::Client,
    #[cfg(test)]
    capability_override: Option<bool>,
}

#[derive(Clone)]
struct CapabilityProbe {
    readiness_url: Url,
    spider_upstream: String,
    api_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityDocument {
    schema: String,
    spider_api_url: String,
    dns_all_answers_enforced: bool,
    redirect_every_hop_enforced: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct NetworkOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl EgressPolicy {
    pub fn from_env() -> Result<Self, String> {
        let trusted_hosts = std::env::var("LIVY_RESOLVER_TRUSTED_SOURCE_HOSTS")
            .ok()
            .map(|value| parse_hosts(&value))
            .unwrap_or_default();
        let allow_private_sources = env_bool("LIVY_RESOLVER_ALLOW_PRIVATE_SOURCES", false)?;
        if allow_private_sources {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "resolver_private_source_override_enabled",
                    "warning": "private and local source destinations are allowed by operator configuration"
                })
            );
        }
        let attestation = optional_env("LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION")?;
        let readiness_url = optional_env("LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL")?;
        let capability = match (attestation, readiness_url) {
            (None, None) => None,
            (Some(attestation), Some(readiness_url)) => {
                if attestation.trim() != SPIDER_EGRESS_ATTESTATION {
                    return Err(format!(
                        "LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION must equal `{SPIDER_EGRESS_ATTESTATION}`"
                    ));
                }
                let allow_insecure_dev =
                    env_bool("LIVY_RESOLVER_ALLOW_INSECURE_SPIDER_DEV", false)?;
                Some(build_capability_probe(
                    spider_client::get_api_url(),
                    &readiness_url,
                    allow_insecure_dev,
                    spider_api_key_from_env()?,
                )?)
            }
            _ => {
                return Err(
                    "LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION and LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL must be configured together"
                        .into(),
                );
            }
        };
        let capability_timeout = Duration::from_secs(env_u64(
            "LIVY_RESOLVER_SPIDER_EGRESS_READINESS_TIMEOUT_SECS",
            DEFAULT_CAPABILITY_TIMEOUT_SECS,
        )?);
        let capability_client = reqwest::Client::builder()
            .timeout(capability_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| format!("cannot build Spider readiness client: {err}"))?;
        Ok(Self {
            allow_private_sources,
            trusted_hosts,
            dns_timeout: Duration::from_secs(env_u64(
                "LIVY_RESOLVER_DNS_TIMEOUT_SECS",
                DEFAULT_DNS_TIMEOUT_SECS,
            )?),
            capability,
            capability_client,
            #[cfg(test)]
            capability_override: None,
        })
    }

    /// Validate the local destination and verify the actual-fetch enforcement capability.
    pub async fn validate_source(&self, source: &str) -> Result<(), FetchError> {
        self.validate_source_preflight(source).await?;
        self.require_actual_fetch_capability().await
    }

    /// Probe the operator-attested enforcement point. This is required before every debit/fetch.
    pub async fn require_actual_fetch_capability(&self) -> Result<(), FetchError> {
        #[cfg(test)]
        if let Some(ready) = self.capability_override {
            return if ready {
                Ok(())
            } else {
                Err(capability_unavailable("test capability is unavailable"))
            };
        }

        let capability = self.capability.as_ref().ok_or_else(|| {
            capability_unavailable("operator egress attestation is not configured")
        })?;
        let mut response = self
            .capability_client
            .get(capability.readiness_url.clone())
            .bearer_auth(&capability.api_key)
            .send()
            .await
            .map_err(|err| capability_unavailable(&format!("readiness probe failed: {err}")))?;
        if !response.status().is_success() {
            return Err(capability_unavailable(&format!(
                "readiness probe returned {}",
                response.status()
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        {
            return Err(capability_unavailable(
                "readiness probe did not return application/json",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CAPABILITY_DOCUMENT_BYTES as u64)
        {
            return Err(capability_unavailable(
                "readiness capability document is too large",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| capability_unavailable(&format!("readiness response failed: {err}")))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_CAPABILITY_DOCUMENT_BYTES {
                return Err(capability_unavailable(
                    "readiness capability document is too large",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        validate_capability_document(&body, &capability.spider_upstream)
    }

    /// Validate syntax, the literal host, and every currently resolved address locally.
    async fn validate_source_preflight(&self, source: &str) -> Result<(), FetchError> {
        validate_source_url(source)?;
        let parsed = Url::parse(source)
            .map_err(|_| FetchError::BadRequest("`source` must be a valid absolute URL".into()))?;
        let host = parsed
            .host()
            .ok_or_else(|| FetchError::BadRequest("`source` must include a host".into()))?;
        let normalized_host = normalize_host(parsed.host_str().unwrap_or_default());

        if self.allow_private_sources || self.trusted_hosts.contains(&normalized_host) {
            return Ok(());
        }

        match host {
            Host::Ipv4(address) => ensure_public_addresses([IpAddr::V4(address)]),
            Host::Ipv6(address) => ensure_public_addresses([IpAddr::V6(address)]),
            Host::Domain(domain) => {
                if is_sensitive_hostname(domain) {
                    return Err(blocked_source());
                }
                let port = parsed.port_or_known_default().unwrap_or(80);
                let resolved = tokio::time::timeout(self.dns_timeout, lookup_host((domain, port)))
                    .await
                    .map_err(|_| {
                        FetchError::BadRequest("`source` hostname resolution timed out".into())
                    })?
                    .map_err(|_| {
                        FetchError::BadRequest("`source` hostname could not be resolved".into())
                    })?;
                let addresses: Vec<IpAddr> = resolved.map(|address| address.ip()).collect();
                ensure_public_addresses(addresses)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(trusted_hosts: &[&str], capability_ready: bool) -> Self {
        Self {
            allow_private_sources: false,
            trusted_hosts: trusted_hosts
                .iter()
                .map(|host| normalize_host(host))
                .collect(),
            dns_timeout: Duration::from_millis(100),
            capability: None,
            capability_client: reqwest::Client::new(),
            capability_override: Some(capability_ready),
        }
    }
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(format!("cannot read {name}: {err}")),
    }
}

fn spider_api_key_from_env() -> Result<String, String> {
    for name in [
        "LIVY_RESOLVER_KEY",
        "SPIDER_API_KEY",
        "SPIDER_KEY",
        "LIVY_KEY",
    ] {
        if let Some(value) = optional_env(name)?
            && !value.trim().is_empty()
        {
            return Ok(value);
        }
    }
    Err("a Spider API key is required for the authenticated egress capability probe".into())
}

fn build_capability_probe(
    spider_api_url: &str,
    readiness_url: &str,
    allow_insecure_dev: bool,
    api_key: String,
) -> Result<CapabilityProbe, String> {
    let (spider_api_url, spider_upstream) = normalize_spider_upstream(spider_api_url)?;
    let readiness_url = parse_http_url(readiness_url, "LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL")?;
    if network_origin(&spider_api_url)? != network_origin(&readiness_url)? {
        return Err(
            "LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL must use the exact SPIDER_API_URL origin; configure a policy proxy as SPIDER_API_URL instead of a separate origin"
                .into(),
        );
    }
    if spider_api_url.scheme() != "https"
        && !(allow_insecure_dev && is_loopback_url(&spider_api_url))
    {
        return Err(
            "SPIDER_API_URL and its capability endpoint must use HTTPS; loopback HTTP requires LIVY_RESOLVER_ALLOW_INSECURE_SPIDER_DEV=true"
                .into(),
        );
    }
    Ok(CapabilityProbe {
        readiness_url,
        spider_upstream,
        api_key,
    })
}

fn parse_http_url(value: &str, name: &str) -> Result<Url, String> {
    let parsed = Url::parse(value.trim()).map_err(|_| format!("{name} must be an absolute URL"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(format!(
            "{name} must be http(s), include a host, and omit credentials and fragments"
        ));
    }
    Ok(parsed)
}

fn normalize_spider_upstream(value: &str) -> Result<(Url, String), String> {
    let parsed = parse_http_url(value, "SPIDER_API_URL")?;
    if parsed.query().is_some() {
        return Err("SPIDER_API_URL must omit query parameters".into());
    }
    let normalized = parsed.as_str().trim_end_matches('/').to_string();
    Ok((parsed, normalized))
}

fn network_origin(url: &Url) -> Result<NetworkOrigin, String> {
    Ok(NetworkOrigin {
        scheme: url.scheme().to_ascii_lowercase(),
        host: url
            .host_str()
            .ok_or_else(|| "URL must include a host".to_string())?
            .trim_end_matches('.')
            .to_ascii_lowercase(),
        port: url
            .port_or_known_default()
            .ok_or_else(|| "URL must include an effective port".to_string())?,
    })
}

fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => normalize_host(domain) == "localhost",
        None => false,
    }
}

fn validate_capability_document(
    body: &[u8],
    expected_spider_upstream: &str,
) -> Result<(), FetchError> {
    let document: CapabilityDocument = serde_json::from_slice(body)
        .map_err(|_| capability_unavailable("readiness capability document is invalid"))?;
    let (_, document_upstream) = normalize_spider_upstream(&document.spider_api_url)
        .map_err(|_| capability_unavailable("capability Spider upstream is invalid"))?;
    if document.schema != SPIDER_EGRESS_CAPABILITY_SCHEMA
        || document_upstream != expected_spider_upstream
        || !document.dns_all_answers_enforced
        || !document.redirect_every_hop_enforced
    {
        return Err(capability_unavailable(
            "readiness capability document does not assert the required Spider egress contract",
        ));
    }
    Ok(())
}

fn parse_hosts(value: &str) -> HashSet<String> {
    value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .map(normalize_host)
        .filter(|host| !host.is_empty())
        .collect()
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn is_sensitive_hostname(host: &str) -> bool {
    let host = normalize_host(host);
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "localhost.localdomain"
        || host == "metadata"
        || host.starts_with("metadata.")
        || host == "instance-data"
        || host.ends_with(".internal")
        || host.ends_with(".local")
}

fn ensure_public_addresses(addresses: impl IntoIterator<Item = IpAddr>) -> Result<(), FetchError> {
    let addresses: Vec<IpAddr> = addresses.into_iter().collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(*address)) {
        return Err(blocked_source());
    }
    Ok(())
}

fn blocked_source() -> FetchError {
    FetchError::BadRequest("`source` must resolve only to public network addresses".into())
}

fn capability_unavailable(detail: &str) -> FetchError {
    eprintln!(
        "{}",
        serde_json::json!({
            "event": "spider_egress_capability_unavailable",
            "detail": detail,
        })
    );
    FetchError::EgressUnavailable(detail.to_string())
}

fn is_public_ip(address: IpAddr) -> bool {
    if let IpAddr::V6(address) = address {
        let octets = address.octets();
        // Deny the entire IPv4-mapped/translatable prefix. Allowing a mapped public IPv4 value
        // creates parser and policy disagreements at downstream network boundaries.
        if octets[..10].iter().all(|octet| *octet == 0) && octets[10] == 0xff && octets[11] == 0xff
        {
            return false;
        }
        if let Some(embedded) = address.to_ipv4() {
            return is_public_ip(IpAddr::V4(embedded));
        }
    }
    let address = address.to_string();
    let mut policy = AntiSSRFPolicy::new(PolicyConfigOptions::ExternalOnlyLatest);
    policy
        .is_network_connection_allowed(&[address.as_str()])
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability_document(spider_api_url: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": SPIDER_EGRESS_CAPABILITY_SCHEMA,
            "spider_api_url": spider_api_url,
            "dns_all_answers_enforced": true,
            "redirect_every_hop_enforced": true,
        }))
        .expect("capability JSON")
    }

    #[test]
    fn capability_endpoint_must_share_the_actual_spider_origin() {
        let error = build_capability_probe(
            "https://api.spider.example/v1",
            "https://unrelated.example/readyz",
            false,
            "test-key".into(),
        )
        .err()
        .expect("separate origin must be rejected");
        assert!(error.contains("exact SPIDER_API_URL origin"));

        assert!(
            build_capability_probe(
                "https://api.spider.example/v1",
                "https://api.spider.example:443/readyz",
                false,
                "test-key".into(),
            )
            .is_ok()
        );
    }

    #[test]
    fn insecure_capability_is_limited_to_explicit_loopback_dev() {
        assert!(
            build_capability_probe(
                "http://127.0.0.1:8080",
                "http://127.0.0.1:8080/readyz",
                false,
                "test-key".into(),
            )
            .is_err()
        );
        assert!(
            build_capability_probe(
                "http://127.0.0.1:8080",
                "http://127.0.0.1:8080/readyz",
                true,
                "test-key".into(),
            )
            .is_ok()
        );
        assert!(
            build_capability_probe(
                "http://api.spider.example",
                "http://api.spider.example/readyz",
                true,
                "test-key".into(),
            )
            .is_err()
        );
    }

    #[test]
    fn capability_document_rejects_plain_or_unrelated_success() {
        let expected = "https://api.spider.example";
        assert!(validate_capability_document(b"ok", expected).is_err());
        assert!(
            validate_capability_document(
                &capability_document("https://unrelated.example"),
                expected,
            )
            .is_err()
        );

        let mut missing_redirect: serde_json::Value =
            serde_json::from_slice(&capability_document(expected)).expect("document");
        missing_redirect["redirect_every_hop_enforced"] = serde_json::json!(false);
        assert!(
            validate_capability_document(
                &serde_json::to_vec(&missing_redirect).expect("document"),
                expected,
            )
            .is_err()
        );

        let mut unknown_field: serde_json::Value =
            serde_json::from_slice(&capability_document(expected)).expect("document");
        unknown_field["unrelated"] = serde_json::json!(true);
        assert!(
            validate_capability_document(
                &serde_json::to_vec(&unknown_field).expect("document"),
                expected,
            )
            .is_err()
        );
        assert!(validate_capability_document(&capability_document(expected), expected).is_ok());
    }

    #[test]
    fn sensitive_ipv4_ranges_are_blocked() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
        ] {
            let address = address.parse().expect("test address");
            assert!(!is_public_ip(IpAddr::V4(address)), "{address}");
        }
        assert!(is_public_ip(IpAddr::V4(
            "93.184.216.34".parse().expect("public address")
        )));
    }

    #[test]
    fn sensitive_ipv6_ranges_are_blocked() {
        for address in [
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "64:ff9b::1",
            "64:ff9b:1::1",
            "2001:2::1",
            "2001:20::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            let address = address.parse().expect("test address");
            assert!(!is_public_ip(IpAddr::V6(address)), "{address}");
        }
        assert!(is_public_ip(IpAddr::V6(
            "2606:2800:220:1:248:1893:25c8:1946"
                .parse()
                .expect("public address")
        )));
    }

    #[test]
    fn entire_ipv4_translatable_prefix_is_blocked() {
        for address in [
            "::ffff:169.254.169.254", // metadata/link-local
            "::ffff:10.0.0.1",        // RFC 1918
            "::ffff:100.64.0.1",      // CGNAT
            "::ffff:93.184.216.34",   // otherwise-public control
            "::ffff:8.8.8.8",         // otherwise-public control
        ] {
            let address = address.parse().expect("test address");
            assert!(!is_public_ip(IpAddr::V6(address)), "{address}");
        }
    }

    #[test]
    fn a_mixed_dns_answer_fails_closed() {
        assert!(
            ensure_public_addresses([
                "93.184.216.34".parse().expect("public"),
                "127.0.0.1".parse().expect("loopback"),
            ])
            .is_err()
        );
    }

    #[tokio::test]
    async fn literal_and_metadata_sources_are_rejected() {
        let policy = EgressPolicy::for_tests(&[], true);
        assert!(
            policy
                .validate_source("http://127.0.0.1/admin")
                .await
                .is_err()
        );
        assert!(
            policy
                .validate_source("http://169.254.169.254/latest/meta-data")
                .await
                .is_err()
        );
        assert!(
            policy
                .validate_source("http://metadata.google.internal/computeMetadata/v1")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn explicit_trusted_host_override_is_narrow() {
        let policy = EgressPolicy::for_tests(&["internal.example"], true);
        assert!(
            policy
                .validate_source("http://internal.example/private")
                .await
                .is_ok()
        );
        assert!(
            policy
                .validate_source("http://127.0.0.1/private")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn actual_fetch_capability_is_default_deny() {
        let policy = EgressPolicy::for_tests(&["example.com"], false);
        let error = policy
            .validate_source("https://example.com/")
            .await
            .expect_err("missing capability must refuse the fetch");
        assert!(matches!(error, FetchError::EgressUnavailable(_)));
    }
}
