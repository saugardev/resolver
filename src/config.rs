//! Runtime limits for the public HTTP surface.

use std::time::Duration;

pub const DEFAULT_PRODUCT_BODY_BYTES: usize = 64 * 1024;
pub const DEFAULT_PRODUCT_TIMEOUT_SECS: u64 = 65;

#[derive(Clone, Debug)]
pub struct SecurityConfig {
    pub product_body_bytes: usize,
    pub product_timeout: Duration,
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
            hsts_enabled: env_bool("LIVY_RESOLVER_HSTS_ENABLED", false)?,
        })
    }
}

fn env_usize(name: &str, default: usize) -> Result<usize, String> {
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

fn env_u64(name: &str, default: u64) -> Result<u64, String> {
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

fn env_bool(name: &str, default: bool) -> Result<bool, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded() {
        assert_eq!(DEFAULT_PRODUCT_BODY_BYTES, 65_536);
        assert_eq!(DEFAULT_PRODUCT_TIMEOUT_SECS, 65);
    }
}
