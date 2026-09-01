use serde::{Deserialize, Serialize};

use super::{
    ExtensionApiInvocationProcessEvidence, ExtensionApiOperationFailure, ExtensionApiVersion,
};

pub const EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-action-invoke-request/v1";
pub const EXTENSION_API_ACTION_INVOKE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-action-invoke-response/v1";
pub const ACTION_CAPABILITY_PREFIX: &str = "action.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiActionInvokeRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiActionInvokeResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ExtensionApiInvocationProcessEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::v1::EXTENSION_API_V1;

    #[test]
    fn action_request_wire_shape_is_versioned() {
        let request = ExtensionApiActionInvokeRequest {
            schema: EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: "registry".to_string(),
            action_id: "release.publish".to_string(),
            project_id: None,
            selected: Vec::new(),
            payload: Some(serde_json::json!({"version": "1.2.3"})),
        };

        assert_eq!(
            serde_json::to_value(request).expect("request JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA,
                "api_version": {"major": 1},
                "extension_id": "registry",
                "action_id": "release.publish",
                "payload": {"version": "1.2.3"}
            })
        );
    }
}
