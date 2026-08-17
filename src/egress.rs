//! Default-deny source egress checks.
//!
//! Spider performs the final fetch, but validating every explicit source here prevents callers
//! from using the resolver as a route to loopback, private, link-local, or metadata services.
//! Every address in a DNS answer is checked on every request so mixed-answer and rebinding-style
//! responses fail closed.

use crate::{
    config::{env_bool, env_u64},
    errors::FetchError,
    types::validate_source_url,
};
use std::{collections::HashSet, net::IpAddr, time::Duration};
use tokio::net::lookup_host;
use url::{Host, Url};

const DEFAULT_DNS_TIMEOUT_SECS: u64 = 3;

#[derive(Clone, Debug)]
pub struct EgressPolicy {
    allow_private_sources: bool,
    trusted_hosts: HashSet<String>,
    dns_timeout: Duration,
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
        Ok(Self {
            allow_private_sources,
            trusted_hosts,
            dns_timeout: Duration::from_secs(env_u64(
                "LIVY_RESOLVER_DNS_TIMEOUT_SECS",
                DEFAULT_DNS_TIMEOUT_SECS,
            )?),
        })
    }

    /// Validate syntax, the literal host, and every currently resolved address.
    pub async fn validate_source(&self, source: &str) -> Result<(), FetchError> {
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
    fn for_tests(trusted_hosts: &[&str]) -> Self {
        Self {
            allow_private_sources: false,
            trusted_hosts: trusted_hosts
                .iter()
                .map(|host| normalize_host(host))
                .collect(),
            dns_timeout: Duration::from_millis(100),
        }
    }
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

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, d] = address.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
                || (a == 255 && b == 255 && c == 255 && d == 255))
        }
        IpAddr::V6(address) => {
            let octets = address.octets();
            if let Some(embedded) = address.to_ipv4() {
                return is_public_ip(IpAddr::V4(embedded));
            }
            !(address.is_unspecified()
                || address.is_loopback()
                || (octets[0] & 0xfe) == 0xfc
                || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
                || octets[0] == 0xff
                || (octets[0..4] == [0x20, 0x01, 0x0d, 0xb8])
                || (octets[0..8] == [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]))
        }
    }
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
            "2001:db8::1",
            "::ffff:127.0.0.1",
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
        let policy = EgressPolicy::for_tests(&[]);
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
        let policy = EgressPolicy::for_tests(&["internal.example"]);
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
}
