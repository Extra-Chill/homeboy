//! The authoritative shape of an agent-task executor provider *as an extension
//! declares it*.
//!
//! `agent_task_executors` is carried opaquely as `Vec<serde_json::Value>` on the
//! runtime manifest so that lower layers stay agnostic of the agent-task
//! provider types. Something still has to decide whether a declaration is
//! well-formed, and that decision has to be made in two places that cannot see
//! each other:
//!
//! - `homeboy-extension`, at install/replace time, so a malformed declaration is
//!   rejected and rolled back instead of installed silently.
//! - `homeboy-agents`, at discovery time, which owns the far richer resolved
//!   provider type.
//!
//! `homeboy-agents` depends on `homeboy-extension`, so `homeboy-extension` can
//! never call into it. Rather than duplicate the rule on both sides (which would
//! drift), the rule lives here once and both sides call it.
//!
//! This type deliberately models only what an extension *declares* and what
//! decides validity. It is not a second copy of the resolved provider: fields
//! such as `extension_id`, `extension_path`, `runtime_id` and `runtime_path` are
//! injected by discovery after parsing, never authored in a manifest, so they
//! have no place in a declaration contract. Everything else an extension may
//! declare is preserved verbatim in `extra` and interpreted by the agent-task
//! layer.

use std::collections::BTreeMap;

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA: &str = "homeboy/agent-task-executor-provider/v1";

fn default_provider_schema() -> String {
    AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string()
}

/// One `agent_runtimes[].agent_task_executors[]` entry as authored in an
/// extension manifest.
///
/// `id` and `backend` are required: they are the identity a provider is selected
/// by, and a declaration missing either cannot be resolved to anything. Every
/// other declared key is retained in `extra` so this contract never rejects a
/// provider feature it does not itself model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorProviderDeclaration {
    #[serde(default = "default_provider_schema")]
    pub schema: String,
    pub id: String,
    pub backend: String,
    /// Rejected whenever non-empty. The string form was replaced by
    /// `invocation.argv` / `argv`; accepting it silently would let a manifest
    /// declare a command that is never executed.
    #[serde(default, deserialize_with = "reject_deprecated_provider_command")]
    pub command: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

fn reject_deprecated_provider_command<'de, D>(
    deserializer: D,
) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let command = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    if command.trim().is_empty() {
        return Ok(command);
    }

    Err(<D::Error as serde::de::Error>::custom(
        "agent-task provider string-form 'command' is no longer supported; use invocation.argv or argv instead",
    ))
}

/// Parse one declared executor entry, producing the canonical diagnostic used by
/// both the install-time gate and agent-task discovery.
///
/// `extension_id` and `runtime_id` only shape the error; they are not part of
/// the declaration.
pub fn parse_agent_task_executor_declaration(
    extension_id: &str,
    runtime_id: &str,
    value: &Value,
) -> Result<AgentTaskExecutorProviderDeclaration> {
    serde_json::from_value(value.clone()).map_err(|err| {
        Error::validation_invalid_argument(
            "agent_runtimes.agent_task_executors",
            format!(
                "Extension '{}' declares an agent runtime provider that cannot be parsed: {}",
                extension_id, err
            ),
            Some(runtime_id.to_string()),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_complete_declaration_parses() {
        let declaration = parse_agent_task_executor_declaration(
            "wordpress",
            "wordpress-runtime",
            &json!({"id": "wordpress.default", "backend": "wp-codebox"}),
        )
        .expect("complete declaration parses");

        assert_eq!(declaration.id, "wordpress.default");
        assert_eq!(declaration.backend, "wp-codebox");
        assert_eq!(declaration.schema, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA);
    }

    #[test]
    fn a_declaration_missing_backend_cannot_be_parsed() {
        let error = parse_agent_task_executor_declaration(
            "wordpress",
            "wordpress-runtime",
            &json!({"id": "wordpress.default"}),
        )
        .expect_err("a declaration without a backend must be rejected");

        assert!(
            error.message.contains("cannot be parsed"),
            "got {}",
            error.message
        );
        assert!(error.message.contains("wordpress"), "got {}", error.message);
    }

    #[test]
    fn a_declaration_missing_id_cannot_be_parsed() {
        let error = parse_agent_task_executor_declaration(
            "wordpress",
            "wordpress-runtime",
            &json!({"backend": "wp-codebox"}),
        )
        .expect_err("a declaration without an id must be rejected");

        assert!(error.message.contains("cannot be parsed"));
    }

    #[test]
    fn the_deprecated_string_command_form_is_rejected() {
        let error = parse_agent_task_executor_declaration(
            "wordpress",
            "wordpress-runtime",
            &json!({"id": "a", "backend": "b", "command": "run --thing"}),
        )
        .expect_err("string-form command must be rejected");

        assert!(error.message.contains("cannot be parsed"));
    }

    #[test]
    fn unmodelled_provider_keys_are_preserved_rather_than_rejected() {
        let declaration = parse_agent_task_executor_declaration(
            "wordpress",
            "wordpress-runtime",
            &json!({
                "id": "a",
                "backend": "b",
                "capabilities": ["x"],
                "some_future_provider_key": {"nested": true}
            }),
        )
        .expect("a declaration carrying keys this contract does not model still parses");

        assert!(declaration.extra.contains_key("capabilities"));
        assert!(declaration.extra.contains_key("some_future_provider_key"));
    }
}
