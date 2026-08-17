//! Runtime limits for the public HTTP surface.

use std::time::Duration;

pub const DEFAULT_PRODUCT_BODY_BYTES: usize = 64 * 1024;
pub const DEFAULT_PRODUCT_TIMEOUT_SECS: u64 = 65;
pub const DEFAULT_MCP_TIMEOUT_SECS: u64 = 65;

const DEFAULT_MCP_ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];
const DEFAULT_MCP_ALLOWED_ORIGINS: &[&str] = &["http://localhost:3000", "http://127.0.0.1:3000"];

#[derive(Clone, Debug)]
pub struct SecurityConfig {
    pub product_body_bytes: usize,
    pub product_timeout: Duration,
    pub mcp_timeout: Duration,
    pub hsts_enabled: bool,
}

impl SecurityConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            product_body_bytes: env_usize(
                "LIVY_RESOLVER_MAX_PRODUCT_BODY_BYTES",
                DEFAULT_PRODUCT_BODY_BYTES,
            )?,
            product_timeout: Duration::from_secs(env_u64(
                "LIVY_RESOLVER_PRODUCT_TIMEOUT_SECS",
                DEFAULT_PRODUCT_TIMEOUT_SECS,
            )?),
            mcp_timeout: Duration::from_secs(env_u64(
                "LIVY_RESOLVER_MCP_TIMEOUT_SECS",
                DEFAULT_MCP_TIMEOUT_SECS,
            )?),
            hsts_enabled: env_bool("LIVY_RESOLVER_HSTS_ENABLED", false)?,
        })
    }
}

/// Inbound MCP transport and session limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpRuntimeConfig {
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
}

impl McpRuntimeConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            allowed_hosts: env_list("LIVY_RESOLVER_MCP_ALLOWED_HOSTS", DEFAULT_MCP_ALLOWED_HOSTS)?,
            allowed_origins: env_list(
                "LIVY_RESOLVER_MCP_ALLOWED_ORIGINS",
                DEFAULT_MCP_ALLOWED_ORIGINS,
            )?,
        })
    }
}

pub(crate) fn env_usize(name: &str, default: usize) -> Result<usize, String> {
    match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(format!("cannot read {name}: {err}")),
    }
}

pub(crate) fn env_u64(name: &str, default: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(format!("cannot read {name}: {err}")),
    }
}

pub(crate) fn env_bool(name: &str, default: bool) -> Result<bool, String> {
    match std::env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(format!("{name} must be a boolean")),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(format!("cannot read {name}: {err}")),
    }
}

fn env_list(name: &str, default: &[&str]) -> Result<Vec<String>, String> {
    let values = match std::env::var(name) {
        Ok(value) => parse_list(&value),
        Err(std::env::VarError::NotPresent) => {
            default.iter().map(|value| (*value).into()).collect()
        }
        Err(err) => return Err(format!("cannot read {name}: {err}")),
    };
    if values.is_empty() {
        return Err(format!("{name} must contain at least one value"));
    }
    Ok(values)
}

fn parse_list(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    for item in value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let item = item.to_ascii_lowercase();
        if !values.contains(&item) {
            values.push(item);
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lists_are_normalized_and_deduplicated() {
        assert_eq!(
            parse_list("Resolver.Example, localhost resolver.example"),
            vec!["resolver.example", "localhost"]
        );
    }
}
