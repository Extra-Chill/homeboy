//! Resolution of the command policy a dispatch (and therefore a cook) runs
//! under.
//!
//! An execution budget bounds how long a provider may run. A command policy
//! bounds *what it may run*. On a resource-constrained host the two are not
//! interchangeable: an agent that spends its whole budget compiling produces
//! zero edits (#11481).

use crate::agent_task::{AgentCommandPolicy, AgentCommandPolicyMode, AgentCommandRule};
use crate::agent_task_dispatch_service::AgentTaskDispatchRequest;
use homeboy_core::{defaults, Error, Result};

/// Resolve the effective command policy for one dispatch.
///
/// The host-level `agent_task.command_policy` config is the base, so a
/// resource-constrained machine can declare "builds go to CI here" once and
/// every cook on it inherits the refusal without a per-invocation flag.
/// Per-dispatch `--deny-command` / `--allow-command` / `--command-policy-reason`
/// **extend** that base; they never silently drop a host rule, because an
/// operator forgetting a flag must not re-open the failure mode the host policy
/// exists to prevent.
pub(super) fn resolve_dispatch_command_policy(
    request: &AgentTaskDispatchRequest,
    managed_worktree: bool,
) -> Result<AgentCommandPolicy> {
    let mut policy = match defaults::load_config().agent_task.command_policy {
        Some(raw) => serde_json::from_value::<AgentCommandPolicy>(raw).map_err(|error| {
            Error::validation_invalid_argument(
                "agent_task.command_policy",
                format!(
                    "configured agent_task.command_policy is not a valid \
homeboy/agent-command-policy/v1 document: {error}"
                ),
                None,
                Some(vec![
                    "Example: homeboy config set /agent_task/command_policy '{\"deny\":[{\"pattern\":\"cargo test\",\"reason\":\"this host routes builds to CI\"}]}' --json".to_string(),
                ]),
            )
        })?,
        None => AgentCommandPolicy::default(),
    };

    policy.deny.extend(
        request
            .core
            .deny_command
            .iter()
            .map(|pattern| AgentCommandRule::new(pattern.clone())),
    );
    policy.allow.extend(
        request
            .core
            .allow_command
            .iter()
            .map(|pattern| AgentCommandRule::new(pattern.clone())),
    );
    // An explicit `--allow-command` is unambiguous intent: only these run.
    if !request.core.allow_command.is_empty() {
        policy.mode = AgentCommandPolicyMode::AllowList;
    }
    if let Some(reason) = &request.core.command_policy_reason {
        policy.reason = Some(reason.clone());
    }
    if managed_worktree {
        policy.deny.push(AgentCommandRule::with_reason(
            "git stash*",
            "Git's stash stack is repository-global and unsafe for parallel managed worktrees; preserve changes with an operation-owned patch artifact",
        ));
    }

    Ok(policy)
}
