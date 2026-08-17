use std::{fs, path::PathBuf, process::Command};
use uuid::Uuid;

#[test]
fn fresh_external_consumer_resolves_and_compiles_public_factory_api() {
    let resolver_path = serde_json::to_string(env!("CARGO_MANIFEST_DIR")).unwrap();
    let consumer_root = std::env::temp_dir().join(format!(
        "livy-resolver-external-consumer-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::create_dir_all(consumer_root.join("src")).unwrap();

    let manifest = format!(
        r#"[package]
name = "livy-resolver-external-consumer"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
async-trait = "0.1.89"
livy-resolver = {{ path = {resolver_path} }}
tokio = {{ version = "1.52.1", features = ["macros", "rt-multi-thread"] }}
"#
    );
    let source = r#"use async_trait::async_trait;
use livy_resolver::{
    Receipt, ReceiptOwner, ReceiptStore, ReceiptStoreError, ReceiptStoreFactory,
    build_app_with_receipt_store_factory,
};
use std::sync::Arc;

struct ExternalStore;

#[async_trait]
impl ReceiptStore for ExternalStore {
    async fn put(
        &self,
        owner: &ReceiptOwner,
        _receipt: Receipt,
    ) -> Result<(), ReceiptStoreError> {
        let _key = owner.storage_key();
        Ok(())
    }

    async fn get(
        &self,
        owner: &ReceiptOwner,
        _receipt_id: &str,
    ) -> Result<Option<Receipt>, ReceiptStoreError> {
        let _tenant = owner.tenant_id();
        let _project = owner.project_id();
        Ok(None)
    }

    fn is_shared_durable(&self) -> bool {
        true
    }

    async fn health_check(&self) -> Result<(), ReceiptStoreError> {
        Ok(())
    }
}

struct ExternalFactory;

#[async_trait]
impl ReceiptStoreFactory for ExternalFactory {
    async fn create(&self) -> Result<Arc<dyn ReceiptStore>, ReceiptStoreError> {
        Ok(Arc::new(ExternalStore))
    }
}

#[tokio::main]
async fn main() {
    let factory = ExternalFactory;
    let _router = build_app_with_receipt_store_factory(&factory).await;
}
"#;
    fs::write(consumer_root.join("Cargo.toml"), manifest).unwrap();
    fs::write(consumer_root.join("src/main.rs"), source).unwrap();

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("external-consumer-resolution");
    let output = Command::new(cargo)
        .args(["check", "--quiet", "--offline"])
        .current_dir(&consumer_root)
        .env("CARGO_TARGET_DIR", target_dir)
        .output()
        .unwrap();
    let resolved_lock = fs::read_to_string(consumer_root.join("Cargo.lock")).unwrap_or_default();
    let cleanup = fs::remove_dir_all(&consumer_root);

    assert!(
        output.status.success(),
        "fresh external consumer failed to compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        resolved_lock.contains("name = \"spider-client\"\nversion = \"0.1.87\""),
        "fresh external resolution did not select spider-client 0.1.87"
    );
    cleanup.expect("remove external consumer fixture");
}
