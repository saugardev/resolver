//! Default resolver binary using the environment-configured receipt store factory.

use livygensyn::{EnvironmentReceiptStoreFactory, Result, run_with_receipt_store_factory};

#[tokio::main]
async fn main() -> Result<()> {
    run_with_receipt_store_factory(&EnvironmentReceiptStoreFactory).await
}
