use crate::core::error::{Error, Result};

use super::RunnerWorkspaceSyncMode;

pub fn preflight_lab_offload_changed_since(
    args: &[String],
    sync_mode: RunnerWorkspaceSyncMode,
) -> Result<()> {
    if sync_mode != RunnerWorkspaceSyncMode::Snapshot {
        return Ok(());
    }

    let Some(git_ref) = lab_offload_changed_since_ref(args) else {
        return Ok(());
    };

    Err(Error::validation_invalid_argument(
        "changed_since",
        "Lab offload cannot honor --changed-since in snapshot workspaces because snapshot sync excludes .git metadata",
        Some(git_ref),
        Some(vec![
            "Use a git-backed Lab workspace sync mode before offloading changed-since commands."
                .to_string(),
            "Run the changed-since command locally when the Lab workspace is snapshot-only."
                .to_string(),
        ]),
    ))
}

fn lab_offload_changed_since_ref(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--changed-since" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--changed-since=") {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_changed_since_before_passthrough_args() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--changed-since=origin/main".to_string(),
            "--".to_string(),
            "--changed-since".to_string(),
            "fixture".to_string(),
        ];

        assert_eq!(
            lab_offload_changed_since_ref(&args),
            Some("origin/main".to_string())
        );
    }

    #[test]
    fn ignores_passthrough_changed_since_args() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--".to_string(),
            "--changed-since".to_string(),
            "fixture".to_string(),
        ];

        assert_eq!(lab_offload_changed_since_ref(&args), None);
    }

    #[test]
    fn rejects_changed_since_for_snapshot_lab_offload() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
        ];

        let err = preflight_lab_offload_changed_since(&args, RunnerWorkspaceSyncMode::Snapshot)
            .expect_err("snapshot Lab offload must reject changed-since");

        assert!(err.message.contains("cannot honor --changed-since"));
        assert!(err.message.contains("snapshot workspaces"));
        assert_eq!(err.details["id"], "origin/main");
    }

    #[test]
    fn allows_changed_since_for_git_lab_offload() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
        ];

        preflight_lab_offload_changed_since(&args, RunnerWorkspaceSyncMode::Git)
            .expect("git Lab offload can preserve changed-since semantics");
    }
}
