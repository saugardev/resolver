//! Tenant-scoped receipt persistence boundary.
//!
//! The built-in store is deliberately bounded and expiring. It is suitable for
//! local development and explicitly accepted single-replica deployments only;
//! every startup rejects it unless the operator explicitly acknowledges that
//! limitation. A
//! shared durable implementation can be injected through [`ReceiptStore`].

use crate::{auth::ResolverAuthContext, types::Receipt};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use thiserror::Error;

pub const DEFAULT_RECEIPT_TTL_SECS: u64 = 15 * 60;
pub const DEFAULT_RECEIPT_CAPACITY: usize = 10_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct ReceiptOwner {
    tenant_id: String,
    project_id: String,
}

impl ReceiptOwner {
    pub fn from_auth_context(
        auth_context: Option<&ResolverAuthContext>,
    ) -> Result<Self, ReceiptStoreError> {
        let Some(context) = auth_context else {
            return Ok(Self::local());
        };

        match (context.tenant_id.as_deref(), context.project_id.as_deref()) {
            (Some(tenant_id), Some(project_id))
                if !tenant_id.trim().is_empty() && !project_id.trim().is_empty() =>
            {
                Ok(Self {
                    tenant_id: tenant_id.to_string(),
                    project_id: project_id.to_string(),
                })
            }
            (None, None) if context.access_token.is_none() => Ok(Self::local()),
            _ => Err(ReceiptStoreError::MissingOwner),
        }
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Collision-free, versioned key suitable for an external receipt store.
    pub fn storage_key(&self) -> String {
        format!(
            "v1:{}:{}:{}:{}",
            self.tenant_id().len(),
            self.tenant_id(),
            self.project_id().len(),
            self.project_id()
        )
    }

    #[cfg(test)]
    fn new(tenant_id: &str, project_id: &str) -> Self {
        Self {
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
        }
    }

    fn local() -> Self {
        Self {
            tenant_id: "local-development".to_string(),
            project_id: "local-development".to_string(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ReceiptStoreError {
    #[error("authenticated receipt access requires tenant and project claims")]
    MissingOwner,
    #[error("receipt store lock is unavailable")]
    LockUnavailable,
    #[error("{0}")]
    InvalidConfiguration(String),
}

#[async_trait]
pub trait ReceiptStore: Send + Sync {
    async fn put(&self, owner: &ReceiptOwner, receipt: Receipt) -> Result<(), ReceiptStoreError>;

    async fn get(
        &self,
        owner: &ReceiptOwner,
        receipt_id: &str,
    ) -> Result<Option<Receipt>, ReceiptStoreError>;

    /// Whether records survive restarts and are shared by every service replica.
    fn is_shared_durable(&self) -> bool;

    /// Verify that the backing store can currently serve reads and writes.
    async fn health_check(&self) -> Result<(), ReceiptStoreError>;
}

#[async_trait]
pub trait ReceiptStoreFactory: Send + Sync {
    async fn create(&self) -> Result<std::sync::Arc<dyn ReceiptStore>, ReceiptStoreError>;
}

#[derive(Debug, Default)]
pub struct EnvironmentReceiptStoreFactory;

#[async_trait]
impl ReceiptStoreFactory for EnvironmentReceiptStoreFactory {
    async fn create(&self) -> Result<std::sync::Arc<dyn ReceiptStore>, ReceiptStoreError> {
        let backend =
            optional_env("LIVY_RESOLVER_RECEIPT_STORE").unwrap_or_else(|| "memory".to_string());
        match backend.as_str() {
            "memory" => Ok(std::sync::Arc::new(InMemoryReceiptStore::from_env()?)),
            _ => Err(ReceiptStoreError::InvalidConfiguration(format!(
                "unsupported LIVY_RESOLVER_RECEIPT_STORE `{backend}`; provide a ReceiptStoreFactory backed by shared durable storage"
            ))),
        }
    }
}

#[derive(Debug)]
struct StoredReceipt {
    owner_storage_key: String,
    receipt: Receipt,
    inserted_at: Instant,
}

#[derive(Debug)]
pub struct InMemoryReceiptStore {
    records: Mutex<HashMap<String, StoredReceipt>>,
    ttl: Duration,
    capacity: usize,
}

impl InMemoryReceiptStore {
    pub fn new(ttl: Duration, capacity: usize) -> Result<Self, ReceiptStoreError> {
        if ttl.is_zero() {
            return Err(ReceiptStoreError::InvalidConfiguration(
                "receipt TTL must be greater than zero".to_string(),
            ));
        }
        if capacity == 0 {
            return Err(ReceiptStoreError::InvalidConfiguration(
                "receipt capacity must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            records: Mutex::new(HashMap::new()),
            ttl,
            capacity,
        })
    }

    pub fn from_env() -> Result<Self, ReceiptStoreError> {
        let ttl_secs = env_u64("LIVY_RESOLVER_RECEIPT_TTL_SECS", DEFAULT_RECEIPT_TTL_SECS)?;
        let capacity = env_usize("LIVY_RESOLVER_RECEIPT_CAPACITY", DEFAULT_RECEIPT_CAPACITY)?;
        Self::new(Duration::from_secs(ttl_secs), capacity)
    }

    fn prune_locked(&self, records: &mut HashMap<String, StoredReceipt>, now: Instant) {
        records.retain(|_, stored| now.duration_since(stored.inserted_at) < self.ttl);
        while records.len() >= self.capacity {
            let Some(oldest_id) = records
                .iter()
                .min_by_key(|(_, stored)| stored.inserted_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            records.remove(&oldest_id);
        }
    }
}

#[async_trait]
impl ReceiptStore for InMemoryReceiptStore {
    async fn put(&self, owner: &ReceiptOwner, receipt: Receipt) -> Result<(), ReceiptStoreError> {
        let now = Instant::now();
        let mut records = self
            .records
            .lock()
            .map_err(|_| ReceiptStoreError::LockUnavailable)?;
        self.prune_locked(&mut records, now);
        records.insert(
            receipt.id.clone(),
            StoredReceipt {
                owner_storage_key: owner.storage_key(),
                receipt,
                inserted_at: now,
            },
        );
        Ok(())
    }

    async fn get(
        &self,
        owner: &ReceiptOwner,
        receipt_id: &str,
    ) -> Result<Option<Receipt>, ReceiptStoreError> {
        let now = Instant::now();
        let mut records = self
            .records
            .lock()
            .map_err(|_| ReceiptStoreError::LockUnavailable)?;
        records.retain(|_, stored| now.duration_since(stored.inserted_at) < self.ttl);
        Ok(records
            .get(receipt_id)
            .filter(|stored| stored.owner_storage_key == owner.storage_key())
            .map(|stored| stored.receipt.clone()))
    }

    fn is_shared_durable(&self) -> bool {
        false
    }

    async fn health_check(&self) -> Result<(), ReceiptStoreError> {
        self.records
            .lock()
            .map(|_| ())
            .map_err(|_| ReceiptStoreError::LockUnavailable)
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn env_u64(name: &str, default: u64) -> Result<u64, ReceiptStoreError> {
    match optional_env(name) {
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                ReceiptStoreError::InvalidConfiguration(format!(
                    "{name} must be a positive integer"
                ))
            }),
        None => Ok(default),
    }
}

fn env_usize(name: &str, default: usize) -> Result<usize, ReceiptStoreError> {
    match optional_env(name) {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                ReceiptStoreError::InvalidConfiguration(format!(
                    "{name} must be a positive integer"
                ))
            }),
        None => Ok(default),
    }
}

pub fn env_bool(name: &str, default: bool) -> Result<bool, ReceiptStoreError> {
    match optional_env(name).as_deref() {
        Some("1" | "true" | "yes" | "on") => Ok(true),
        Some("0" | "false" | "no" | "off") => Ok(false),
        Some(_) => Err(ReceiptStoreError::InvalidConfiguration(format!(
            "{name} must be a boolean"
        ))),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(id: &str, created_at_unix_ms: u128) -> Receipt {
        Receipt {
            id: id.to_string(),
            source_url: "https://example.com".to_string(),
            mode: "fetch:fast".to_string(),
            request_type: "smart".to_string(),
            proxy: None,
            status: Some(200),
            error: None,
            duration_elapsed_ms: Some(1),
            content_bytes: Some(10),
            total_cost: None,
            created_at_unix_ms,
            demo_message: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn receipt_access_is_tenant_and_project_scoped() {
        let store = InMemoryReceiptStore::new(Duration::from_secs(60), 10).unwrap();
        let owner = ReceiptOwner::new("tenant-a", "project-a");
        store.put(&owner, receipt("opaque-id", 1)).await.unwrap();

        assert!(store.get(&owner, "opaque-id").await.unwrap().is_some());
        assert!(
            store
                .get(&ReceiptOwner::new("tenant-b", "project-a"), "opaque-id")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get(&ReceiptOwner::new("tenant-a", "project-b"), "opaque-id")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn receipt_store_expires_and_caps_records() {
        let owner = ReceiptOwner::new("tenant-a", "project-a");
        let capped = InMemoryReceiptStore::new(Duration::from_secs(60), 2).unwrap();
        capped.put(&owner, receipt("one", 1)).await.unwrap();
        capped.put(&owner, receipt("two", 2)).await.unwrap();
        capped.put(&owner, receipt("three", 3)).await.unwrap();
        assert!(capped.get(&owner, "one").await.unwrap().is_none());
        assert!(capped.get(&owner, "two").await.unwrap().is_some());
        assert!(capped.get(&owner, "three").await.unwrap().is_some());

        let expiring = InMemoryReceiptStore::new(Duration::from_millis(1), 2).unwrap();
        expiring.put(&owner, receipt("old", 1)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        assert!(expiring.get(&owner, "old").await.unwrap().is_none());
    }

    #[test]
    fn receipt_owner_exposes_an_unambiguous_external_storage_key() {
        let owner = ReceiptOwner::new("tenant:a", "project:b");
        assert_eq!(owner.tenant_id(), "tenant:a");
        assert_eq!(owner.project_id(), "project:b");
        assert_eq!(owner.storage_key(), "v1:8:tenant:a:9:project:b");
        assert_eq!(
            serde_json::to_value(&owner).unwrap(),
            serde_json::json!({"tenant_id": "tenant:a", "project_id": "project:b"})
        );
    }

    #[test]
    fn authenticated_receipt_owner_requires_both_scope_claims() {
        let context = ResolverAuthContext {
            access_token: Some("token".to_string()),
            subject: Some("user-a".to_string()),
            client_id: Some("client".to_string()),
            scopes: vec![],
            audiences: vec![],
            tenant_id: Some("tenant-a".to_string()),
            project_id: None,
        };

        assert!(matches!(
            ReceiptOwner::from_auth_context(Some(&context)),
            Err(ReceiptStoreError::MissingOwner)
        ));
    }
}
