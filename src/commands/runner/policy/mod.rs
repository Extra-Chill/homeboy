use homeboy::core::server::RunnerPolicy;
use homeboy::core::MergeOutput;

use super::{runner, CmdResult, RunnerOutput};

#[derive(Default)]
pub(super) struct RunnerPolicyPatch {
    pub(super) accepted_peer_ids: Vec<String>,
    pub(super) accepted_peer_fingerprints: Vec<String>,
    pub(super) allowed_projects: Vec<String>,
    pub(super) allowed_commands: Vec<String>,
    pub(super) allow_raw_exec: Option<bool>,
    pub(super) workspace_roots: Vec<String>,
    pub(super) artifact_policy: Option<String>,
}

impl RunnerPolicyPatch {
    pub(super) fn trust(
        peers: Vec<String>,
        fingerprints: Vec<String>,
        projects: Vec<String>,
        commands: Vec<String>,
        allow_raw_exec: Option<bool>,
        workspace_roots: Vec<String>,
        artifact_policy: Option<String>,
    ) -> Self {
        Self {
            accepted_peer_ids: peers,
            accepted_peer_fingerprints: fingerprints,
            allowed_projects: projects,
            allowed_commands: commands,
            allow_raw_exec,
            workspace_roots,
            artifact_policy,
        }
    }

    pub(super) fn pair(
        peers: Vec<String>,
        fingerprints: Vec<String>,
        projects: Vec<String>,
        allow_raw_exec: Option<bool>,
        workspace_roots: Vec<String>,
    ) -> Self {
        Self {
            accepted_peer_ids: peers,
            accepted_peer_fingerprints: fingerprints,
            allowed_projects: projects,
            allowed_commands: Vec::new(),
            allow_raw_exec,
            workspace_roots,
            artifact_policy: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.accepted_peer_ids.is_empty()
            && self.accepted_peer_fingerprints.is_empty()
            && self.allowed_projects.is_empty()
            && self.allowed_commands.is_empty()
            && self.allow_raw_exec.is_none()
            && self.workspace_roots.is_empty()
            && self.artifact_policy.is_none()
    }
}

pub(super) fn update(
    runner_id: &str,
    patch: RunnerPolicyPatch,
    command: &str,
) -> CmdResult<RunnerOutput> {
    if patch.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "policy",
            "Provide at least one trust policy field to update",
            Some(runner_id.to_string()),
            None,
        ));
    }

    let current = runner::load(runner_id)?;
    let mut policy = current.policy.clone();
    apply_patch(&mut policy, patch);

    let spec = serde_json::json!({ "policy": policy });
    match runner::merge(
        Some(runner_id),
        &homeboy::core::config::to_json_string(&spec)?,
        &[],
    )? {
        MergeOutput::Single(result) => single_output(command, result.id),
        MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                RunnerOutput {
                    command: command.to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn apply_patch(policy: &mut RunnerPolicy, patch: RunnerPolicyPatch) {
    extend_unique(&mut policy.accepted_peer_ids, patch.accepted_peer_ids);
    extend_unique(
        &mut policy.accepted_peer_fingerprints,
        patch.accepted_peer_fingerprints,
    );
    extend_unique(&mut policy.allowed_projects, patch.allowed_projects);
    extend_unique(&mut policy.allowed_commands, patch.allowed_commands);
    extend_unique(&mut policy.workspace_roots, patch.workspace_roots);
    if let Some(allow_raw_exec) = patch.allow_raw_exec {
        policy.allow_raw_exec = Some(allow_raw_exec);
    }
    if let Some(artifact_policy) = patch.artifact_policy {
        policy.artifact_policy = Some(artifact_policy);
    }
}

fn extend_unique(existing: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !existing.contains(&value) {
            existing.push(value);
        }
    }
}

fn single_output(command: &str, id: String) -> CmdResult<RunnerOutput> {
    let entity = runner::load(&id)?;
    Ok((
        RunnerOutput {
            command: command.to_string(),
            id: Some(id),
            entity: Some(entity),
            updated_fields: vec!["policy".to_string()],
            ..Default::default()
        },
        0,
    ))
}
