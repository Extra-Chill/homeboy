use super::{
    ActivityCollector, ActivityContext, ActivityCrossRefs, ActivityEvidenceRef, ActivityFilter,
    ActivityItem, ActivityRunnerRefs, ActivityState, ActivityTaskIdentity,
    WORKTREE_RESOURCE_SOURCE_STORE,
};
use crate::worktree_provider::{self, WorktreeWorkspace};
use crate::Result;

pub(super) fn collect(collector: &mut ActivityCollector, filter: &ActivityFilter) -> Result<()> {
    for workspace in worktree_provider::list_worktree_inventory()? {
        let item = item_from_workspace(workspace);
        if filter.matches(&item) {
            collector.insert(item);
        }
    }
    Ok(())
}

fn item_from_workspace(workspace: WorktreeWorkspace) -> ActivityItem {
    let provider_id = "native";
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
        // Marks every item from this source as open-resource inventory for
        // the work classification in `model` (#13620).
        source_store: WORKTREE_RESOURCE_SOURCE_STORE.to_string(),
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
            id: provider_id.to_string(),
            kind: "worktree-registry".to_string(),
            uri: "homeboy://worktree-registry/native".to_string(),
        }],
        source_projections: Vec::new(),
        state_conflicts: Vec::new(),
        next_actions: Vec::new(),
        failure: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::ActivityWorkClass;
    use crate::worktree_provider::{
        WorktreeOwnership, WorktreeSafety, WorktreeWorkspace, WorktreeWorkspaceKind,
    };

    #[test]
    fn native_workspace_projects_owner_and_identity_into_activity() {
        let item = item_from_workspace(WorktreeWorkspace {
            ownership: WorktreeOwnership {
                handle: "repo@branch".to_string(),
                path: "/workspace".to_string(),
                kind: WorktreeWorkspaceKind::TaskWorktree,
                branch: Some("branch".to_string()),
                task_url: Some("https://example.test/issues/8017".to_string()),
                provenance: None,
            },
            repository: Some("repo".to_string()),
            owner_run_ref: Some("run-8017".to_string()),
            created_at: Some("2026-08-25T00:00:00Z".to_string()),
            terminal_disposition: None,
            safety: WorktreeSafety {
                dirty: false,
                unpushed: false,
                primary: false,
                missing: false,
            },
        });

        assert_eq!(item.state, ActivityState::Running);
        assert_eq!(item.refs.run_id.as_deref(), Some("run-8017"));
        assert_eq!(item.context.worktree.as_deref(), Some("repo@branch"));
        assert_eq!(item.evidence[0].id, "native");
    }

    /// #13620: an open worktree stays visible with its held state, but the
    /// item itself classifies as open-resource inventory so report counts can
    /// separate it from executing work.
    #[test]
    fn open_worktree_classifies_as_resource_not_executing_work() {
        let workspace = |handle: &str, disposition: Option<&str>| WorktreeWorkspace {
            ownership: WorktreeOwnership {
                handle: handle.to_string(),
                path: format!("/workspace/{handle}"),
                kind: WorktreeWorkspaceKind::AdoptedWorkspace,
                branch: None,
                task_url: None,
                provenance: None,
            },
            repository: None,
            owner_run_ref: None,
            created_at: None,
            terminal_disposition: disposition.map(str::to_string),
            safety: WorktreeSafety {
                dirty: false,
                unpushed: false,
                primary: false,
                missing: false,
            },
        };

        let open = item_from_workspace(workspace("repo@fix-13620", None));
        assert_eq!(open.state, ActivityState::Running);
        assert_eq!(open.work_class(), ActivityWorkClass::OpenResource);
        assert!(open.is_open_resource());
        assert!(!open.is_executing_work());

        // A terminally disposed workspace is closed history, not held
        // inventory, and still classifies as a resource record.
        let disposed = item_from_workspace(workspace("repo@merged", Some("succeeded")));
        assert_eq!(disposed.state, ActivityState::Succeeded);
        assert!(disposed.is_open_resource());
    }
}
