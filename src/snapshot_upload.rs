use crate::errors::SnapshotError;
use crate::provenance::ProvenanceResult;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotPayload {
    pub source_url: String,
    pub receipt_id: String,
    pub html: String,
    pub screenshot_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_error: Option<String>,
}

impl SnapshotPayload {
    pub fn validate_spider_response(response: &Value) -> Result<(), SnapshotError> {
        find_string_by_keys(response, &["raw", "html"])
            .or_else(|| find_content_string(response))
            .ok_or(SnapshotError::MissingHtml)?;
        find_string_by_keys(response, &["screenshot"]).ok_or(SnapshotError::MissingScreenshot)?;
        Ok(())
    }

    pub fn from_spider_response(
        source_url: &str,
        receipt_id: String,
        response: Value,
        provenance: Option<ProvenanceResult>,
        provenance_error: Option<String>,
    ) -> Result<Self, SnapshotError> {
        let html = find_string_by_keys(&response, &["raw", "html"])
            .or_else(|| find_content_string(&response))
            .ok_or(SnapshotError::MissingHtml)?;
        let screenshot_base64 = find_string_by_keys(&response, &["screenshot"])
            .ok_or(SnapshotError::MissingScreenshot)?;

        Ok(Self {
            source_url: source_url.to_string(),
            receipt_id,
            html,
            screenshot_base64,
            provenance,
            provenance_error,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uses_the_resolver_owned_receipt_id() {
        let snapshot = SnapshotPayload::from_spider_response(
            "https://example.com",
            "resolver-generated-id".to_string(),
            json!({
                "id": "predictable-upstream-id",
                "raw": "<html></html>",
                "screenshot": "c2NyZWVuc2hvdA=="
            }),
            None,
            None,
        )
        .unwrap();

        assert_eq!(snapshot.receipt_id, "resolver-generated-id");
    }

    #[test]
    fn validates_required_snapshot_fields_before_side_effects() {
        assert!(SnapshotPayload::validate_spider_response(&json!({"raw": "html"})).is_err());
        assert!(
            SnapshotPayload::validate_spider_response(&json!({
                "raw": "html",
                "screenshot": "bytes"
            }))
            .is_ok()
        );
    }
}
