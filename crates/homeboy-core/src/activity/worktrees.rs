use super::{
    ActivityCollector, ActivityContext, ActivityCrossRefs, ActivityEvidenceRef, ActivityFilter,
    ActivityItem, ActivityRunnerRefs, ActivityState, ActivityTaskIdentity,
};
use crate::worktree_provider::{self, WorktreeProviderIdentity, WorktreeProviderWorkspace};
use crate::Result;

pub(super) fn collect(collector: &mut ActivityCollector, filter: &ActivityFilter) -> Result<()> {
    for workspace in worktree_provider::list_worktree_provider_inventory()? {
        let item = item_from_workspace(workspace);
        if filter.matches(&item) {
            collector.insert(item);
        }
    }
    Ok(())
}

fn item_from_workspace(workspace: WorktreeProviderWorkspace) -> ActivityItem {
    let provider_id = match &workspace.ownership.provider {
        WorktreeProviderIdentity::Native => "native".to_string(),
        WorktreeProviderIdentity::Configured(provider_id) => provider_id.clone(),
    };
    let state = match workspace.terminal_disposition.as_deref() {
        Some("succeeded") => ActivityState::Succeeded,
        Some("failed") => ActivityState::Failed,
        Some("cancelled") => ActivityState::Cancelled,
        Some("timed_out") => ActivityState::TimedOut,
        Some("interrupted") => ActivityState::Failed,
        Some(_) => ActivityState::Unknown,
        None if workspace.safety.missing => ActivityState::Stale,
        None => ActivityState::Running,
    };
    let handle = workspace.ownership.handle.clone();
    let task_url = workspace.ownership.task_url.clone();
    let repository = workspace.repository.clone();
    ActivityItem {
        id: format!("worktree:{provider_id}:{handle}"),
        kind: "worktree".to_string(),
        source_store: "worktree.provider".to_string(),
        state,
        created_at: workspace
            .created_at
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
        updated_at: None,
        finished_at: None,
        command: None,
        cwd: Some(workspace.ownership.path),
        runner: ActivityRunnerRefs::default(),
        refs: ActivityCrossRefs {
            run_id: workspace.owner_run_ref,
            ..Default::default()
        },
        context: ActivityContext {
            task_url: task_url.clone(),
            repository: repository.clone(),
            worktree: Some(handle.clone()),
            identities: vec![ActivityTaskIdentity {
                task_url,
                repository,
                worktree: Some(handle),
            }],
        },
        artifacts: Vec::new(),
        evidence: vec![ActivityEvidenceRef {
            id: provider_id.clone(),
            kind: "worktree-provider".to_string(),
            uri: format!("homeboy://worktree-provider/{provider_id}"),
        }],
        source_projections: Vec::new(),
        state_conflicts: Vec::new(),
        next_actions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree_provider::{
        WorktreeOwnership, WorktreeProviderSafety, WorktreeProviderWorkspace,
    };

    #[test]
    fn provider_workspace_projects_owner_and_identity_into_activity() {
        let item = item_from_workspace(WorktreeProviderWorkspace {
            ownership: WorktreeOwnership {
                provider: WorktreeProviderIdentity::Configured("fixture".to_string()),
                handle: "repo@branch".to_string(),
                path: "/workspace".to_string(),
                branch: "branch".to_string(),
                task_url: Some("https://example.test/issues/8017".to_string()),
            },
            repository: Some("repo".to_string()),
            owner_run_ref: Some("run-8017".to_string()),
            created_at: Some("2026-08-25T00:00:00Z".to_string()),
            terminal_disposition: None,
            safety: WorktreeProviderSafety {
                dirty: false,
                unpushed: false,
                primary: false,
                missing: false,
            },
        });

        assert_eq!(item.state, ActivityState::Running);
        assert_eq!(item.refs.run_id.as_deref(), Some("run-8017"));
        assert_eq!(item.context.worktree.as_deref(), Some("repo@branch"));
        assert_eq!(item.evidence[0].id, "fixture");
    }
}
