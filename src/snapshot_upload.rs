use crate::errors::SnapshotError;
use serde::Serialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotPayload {
    pub source_url: String,
    pub receipt_id: String,
    pub html: String,
    pub screenshot_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::provenance::ProvenanceResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_error: Option<String>,
}

impl SnapshotPayload {
    pub fn from_spider_response(source_url: &str, response: Value) -> Result<Self, SnapshotError> {
        let html = find_string_by_keys(&response, &["raw", "html"])
            .or_else(|| find_content_string(&response))
            .ok_or(SnapshotError::MissingHtml)?;
        let screenshot_base64 = find_string_by_keys(&response, &["screenshot"])
            .ok_or(SnapshotError::MissingScreenshot)?;
        let receipt_id = find_string_by_keys(
            &response,
            &["receipt_id", "receiptId", "request_id", "requestId", "id"],
        )
        .unwrap_or_else(new_receipt_id);

        Ok(Self {
            source_url: source_url.to_string(),
            receipt_id,
            html,
            screenshot_base64,
            provenance: None,
            provenance_error: None,
        })
    }
}

fn find_string_by_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key).and_then(value_to_string) {
                    return Some(found);
                }
            }

            map.values()
                .find_map(|nested| find_string_by_keys(nested, keys))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| find_string_by_keys(nested, keys)),
        _ => None,
    }
}

fn find_content_string(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(content) = map.get("content").and_then(value_to_string) {
                return Some(content);
            }

            map.values().find_map(find_content_string)
        }
        Value::Array(items) => items.iter().find_map(find_content_string),
        _ => None,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    value.as_str().map(ToString::to_string)
}

fn new_receipt_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("snapshot-{millis}")
}
