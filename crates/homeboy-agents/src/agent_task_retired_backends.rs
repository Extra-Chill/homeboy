//! Retired agent-task coding backends (#15932).
//!
//! Pi is Cook's coding backend. OpenCode was retired because it kept a
//! provider-owned credential store outside the Roadie credential pool, and
//! stale `opencode` routes sent cooks down a dead route
//! (`credentials_unusable`). A retired backend is refused at validation, not
//! aliased: config that still names one fails with a clear remediation
//! instead of dispatching.

use homeboy_core::{Error, Result};

use crate::agent_task_scheduler::AgentTaskProviderRotationPolicy;

/// The built-in coding backend Cook selects when no policy names one and the
/// provider catalog declares it.
pub const BUILTIN_DEFAULT_BACKEND: &str = "pi";

/// Backends Homeboy no longer routes agent tasks through.
pub const RETIRED_BACKENDS: &[&str] = &["opencode"];

/// True when `backend` names a retired coding backend.
pub fn is_retired_backend(backend: &str) -> bool {
    let backend = backend.trim();
    RETIRED_BACKENDS
        .iter()
        .any(|retired| retired.eq_ignore_ascii_case(backend))
}

/// Refuse a retired backend named by `field` with a clear remediation.
pub fn reject_retired_backend(field: &str, backend: &str) -> Result<()> {
    if !is_retired_backend(backend) {
        return Ok(());
    }
    let mut error = Error::validation_invalid_argument(
        field,
        format!(
            "agent-task backend `{}` is retired; Pi is the coding backend",
            backend.trim()
        ),
        Some(backend.trim().to_string()),
        Some(vec![
            format!(
                "Replace `{}` with `{BUILTIN_DEFAULT_BACKEND}` in agent_task.default_backend, agent_task.rotation entries, and --backend.",
                backend.trim()
            ),
            format!(
                "Pi routes bill through the Roadie credential pool, for example: homeboy config set /agent_task/rotation '{{\"entries\":[{{\"backend\":\"{BUILTIN_DEFAULT_BACKEND}\",\"model\":\"roadie/anthropic-claude-sonnet-5-5\"}}]}}' --json"
            ),
            "Run `homeboy agent-task providers --validate-readiness` to confirm the Pi routes are verified.".to_string(),
        ]),
    );
    error.details["retired_backend"] = serde_json::Value::String(backend.trim().to_string());
    Err(error)
}

/// Refuse a rotation policy whose entries name a retired backend.
pub fn reject_retired_rotation_backends(
    field: &str,
    rotation: &AgentTaskProviderRotationPolicy,
) -> Result<()> {
    for (index, entry) in rotation.entries.iter().enumerate() {
        if let Some(backend) = entry.backend.as_deref() {
            reject_retired_backend(&format!("{field}.entries[{index}].backend"), backend)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_scheduler::AgentTaskProviderRotationEntry;

    #[test]
    fn opencode_is_retired_and_pi_is_not() {
        assert!(is_retired_backend("opencode"));
        assert!(is_retired_backend(" OpenCode "));
        assert!(!is_retired_backend("pi"));
        assert!(!is_retired_backend("codex"));
    }

    #[test]
    fn retired_backend_fails_validation_with_a_pi_remediation() {
        let error = reject_retired_backend("backend", "opencode").expect_err("retired");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert!(error.message.contains("`opencode` is retired"), "{}", error.message);
        assert_eq!(error.details["retired_backend"], "opencode");
        assert!(error.details["tried"][0]
            .as_str()
            .expect("remediation")
            .contains("`pi`"));
        reject_retired_backend("backend", "pi").expect("pi is the coding backend");
    }

    #[test]
    fn rotation_entry_naming_a_retired_backend_fails_validation() {
        let rotation = AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    backend: Some("pi".to_string()),
                    ..AgentTaskProviderRotationEntry::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    ..AgentTaskProviderRotationEntry::default()
                },
            ],
            ..AgentTaskProviderRotationPolicy::default()
        };
        let error =
            reject_retired_rotation_backends("agent_task.rotation", &rotation).expect_err("retired");
        assert_eq!(
            error.details["field"].as_str(),
            Some("agent_task.rotation.entries[1].backend")
        );
    }
}
