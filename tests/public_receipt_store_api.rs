use async_trait::async_trait;
use livy_resolver::{
    Receipt, ReceiptOwner, ReceiptStore, ReceiptStoreError, ReceiptStoreFactory,
    build_app_with_receipt_store_factory, run_with_receipt_store_factory,
};
use std::sync::Arc;

struct ExternalDurableStore;

#[async_trait]
impl ReceiptStore for ExternalDurableStore {
    async fn put(&self, owner: &ReceiptOwner, _receipt: Receipt) -> Result<(), ReceiptStoreError> {
        let _tenant = owner.tenant_id();
        let _project = owner.project_id();
        let _storage_key = owner.storage_key();
        Ok(())
    }

    async fn get(
        &self,
        owner: &ReceiptOwner,
        _receipt_id: &str,
    ) -> Result<Option<Receipt>, ReceiptStoreError> {
        let _storage_key = owner.storage_key();
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
        Ok(Arc::new(ExternalDurableStore))
    }
}

#[tokio::test]
async fn public_storage_and_application_seams_are_externally_implementable() {
    let factory = ExternalFactory;
    let store = factory.create().await.expect("external factory");
    store.health_check().await.expect("external store health");

    let _ = build_app_with_receipt_store_factory;
    let _ = run_with_receipt_store_factory;
}
