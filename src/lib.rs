//! Embeddable Livy resolver application and data-boundary APIs.

pub mod api;
pub mod app;
pub mod auth;
pub mod config;
pub mod credits;
pub mod egress;
pub mod errors;
pub mod fetch;
pub mod lifecycle;
pub mod mcp;
pub mod provenance;
pub mod receipt_store;
pub mod security;
pub mod snapshot_upload;
pub mod types;

pub use app::{build_app_with_receipt_store_factory, run_with_receipt_store_factory};
pub use errors::{FetchError, Result};
pub use receipt_store::{
    EnvironmentReceiptStoreFactory, ReceiptOwner, ReceiptStore, ReceiptStoreError,
    ReceiptStoreFactory,
};
pub use types::Receipt;
