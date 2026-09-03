//! Versioned, behavior-free materialized rig resource contracts.

use serde::{Deserialize, Serialize};

pub const MATERIALIZED_RIG_RESOURCE_SCHEMA: &str = "homeboy/materialized-rig/v1";

/// A normalized rig document paired with its stable identity and content digest.
///
/// `materialized_rig_json_sha256` is the SHA-256 of the canonical JSON bytes of
/// `rig`: object keys are recursively sorted while array order is preserved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterializedRigResource {
    pub schema: String,
    pub rig_id: String,
    pub materialized_rig_json_sha256: String,
    pub rig: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn materialized_rig_resource_has_a_versioned_golden_wire_shape() {
        let resource = MaterializedRigResource {
            schema: MATERIALIZED_RIG_RESOURCE_SCHEMA.to_string(),
            rig_id: "example".to_string(),
            materialized_rig_json_sha256: "sha256:abc123".to_string(),
            rig: json!({ "id": "example", "settings": { "enabled": true } }),
        };

        let value = serde_json::to_value(&resource).expect("serialize resource");
        assert_eq!(
            value,
            json!({
                "schema": "homeboy/materialized-rig/v1",
                "rig_id": "example",
                "materialized_rig_json_sha256": "sha256:abc123",
                "rig": { "id": "example", "settings": { "enabled": true } }
            })
        );
        assert_eq!(
            serde_json::from_value::<MaterializedRigResource>(value).expect("deserialize resource"),
            resource
        );
    }

    #[test]
    fn materialized_rig_resource_requires_schema_and_identity() {
        let error = serde_json::from_value::<MaterializedRigResource>(json!({
            "materialized_rig_json_sha256": "sha256:abc123",
            "rig": { "id": "example" }
        }))
        .expect_err("schema and rig identity are required");

        assert!(error.to_string().contains("missing field `schema`"));
    }
}
