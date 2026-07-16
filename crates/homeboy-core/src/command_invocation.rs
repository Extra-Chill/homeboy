use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const COMMAND_INVOCATION_SCHEMA: &str = "homeboy/command-invocation/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CommandInvocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<CommandEnvRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "CommandRedaction::is_empty")]
    pub redaction: CommandRedaction,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl CommandInvocation {
    pub fn is_empty(&self) -> bool {
        self.schema.is_none()
            && self.argv.is_empty()
            && self.cwd.is_none()
            && self.env.is_empty()
            && self.display.is_none()
            && self.redaction.is_empty()
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CommandEnvRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CommandRedaction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv_indices: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl CommandRedaction {
    pub fn is_empty(&self) -> bool {
        self.argv_indices.is_empty()
            && self.env.is_empty()
            && self.replacement.is_none()
            && self.extra.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_invocation_contract_accepts_argv_cwd_env_display_and_redaction() {
        let invocation: CommandInvocation = serde_json::from_value(json!({
            "schema": COMMAND_INVOCATION_SCHEMA,
            "argv": ["provider", "--token", "secret-ref"],
            "cwd": "{{runtime_path}}",
            "env": [{ "name": "TOKEN", "source": "secret_env", "redacted": true }],
            "display": "provider --token [redacted]",
            "redaction": { "argv_indices": [2], "env": ["TOKEN"] },
            "provider_note": "preserved"
        }))
        .expect("command invocation parses");

        assert_eq!(
            invocation.schema.as_deref(),
            Some(COMMAND_INVOCATION_SCHEMA)
        );
        assert_eq!(invocation.argv, vec!["provider", "--token", "secret-ref"]);
        assert_eq!(invocation.cwd.as_deref(), Some("{{runtime_path}}"));
        assert_eq!(invocation.env[0].name, "TOKEN");
        assert_eq!(
            invocation.display.as_deref(),
            Some("provider --token [redacted]")
        );
        assert_eq!(invocation.redaction.argv_indices, vec![2]);
        assert_eq!(invocation.extra["provider_note"], "preserved");
    }
}
