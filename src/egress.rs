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
use std::{collections::HashSet, net::IpAddr, time::Duration};
use tokio::net::lookup_host;
use url::{Host, Url};

const DEFAULT_DNS_TIMEOUT_SECS: u64 = 3;
const DEFAULT_CAPABILITY_TIMEOUT_SECS: u64 = 3;
pub const SPIDER_EGRESS_ATTESTATION: &str = "spider-egress-policy-v1";

#[derive(Clone, Debug)]
pub struct EgressPolicy {
    allow_private_sources: bool,
    trusted_hosts: HashSet<String>,
    dns_timeout: Duration,
    capability_readiness_url: Option<Url>,
    capability_client: reqwest::Client,
    #[cfg(test)]
    capability_override: Option<bool>,
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
        let attestation = std::env::var("LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION").ok();
        if let Some(value) = attestation.as_deref()
            && value.trim() != SPIDER_EGRESS_ATTESTATION
        {
            return Err(format!(
                "LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION must equal `{SPIDER_EGRESS_ATTESTATION}`"
            ));
        }
        let readiness_url = std::env::var("LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL")
            .ok()
            .map(|value| parse_readiness_url(&value))
            .transpose()?;
        if attestation.is_some() != readiness_url.is_some() {
            return Err(
                "LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION and LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL must be configured together"
                    .into(),
            );
        }
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
            capability_readiness_url: readiness_url,
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

        let url = self.capability_readiness_url.as_ref().ok_or_else(|| {
            capability_unavailable("operator egress attestation is not configured")
        })?;
        let response = self
            .capability_client
            .get(url.clone())
            .send()
            .await
            .map_err(|err| capability_unavailable(&format!("readiness probe failed: {err}")))?;
        if !response.status().is_success() {
            return Err(capability_unavailable(&format!(
                "readiness probe returned {}",
                response.status()
            )));
        }
        Ok(())
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
            capability_readiness_url: None,
            capability_client: reqwest::Client::new(),
            capability_override: Some(capability_ready),
        }
    }
}

fn parse_readiness_url(value: &str) -> Result<Url, String> {
    let parsed = Url::parse(value.trim()).map_err(|_| {
        "LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL must be an absolute URL".to_string()
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(
            "LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL must be http(s), include a host, and omit credentials"
                .into(),
        );
    }
    Ok(parsed)
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
    if let IpAddr::V6(address) = address
        && let Some(embedded) = address.to_ipv4()
    {
        return is_public_ip(IpAddr::V4(embedded));
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
