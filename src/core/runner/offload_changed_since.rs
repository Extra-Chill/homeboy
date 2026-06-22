use std::path::Path;
use std::process::Command;

use crate::core::error::{Error, Result};

use super::origin_refs::{advertised_origin_refs_for_commit, best_advertised_ref};
use super::RunnerWorkspaceSyncMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabOffloadChangedSincePreflight {
    pub args: Vec<String>,
    pub requested_ref: Option<String>,
    pub resolved_base: Option<String>,
    pub git_fetch_refs: Vec<String>,
}

pub fn preflight_lab_offload_changed_since(
    args: &[String],
    sync_mode: RunnerWorkspaceSyncMode,
) -> Result<LabOffloadChangedSincePreflight> {
    let Some(git_ref) = lab_offload_changed_since_ref(args) else {
        return Ok(LabOffloadChangedSincePreflight {
            args: args.to_vec(),
            requested_ref: None,
            resolved_base: None,
            git_fetch_refs: Vec::new(),
        });
    };

    if sync_mode != RunnerWorkspaceSyncMode::Snapshot {
        return Ok(LabOffloadChangedSincePreflight {
            args: args.to_vec(),
            requested_ref: Some(git_ref.clone()),
            resolved_base: Some(git_ref),
            git_fetch_refs: Vec::new(),
        });
    }

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

pub fn prepare_git_lab_offload_changed_since(
    args: &[String],
    source_path: &Path,
) -> Result<LabOffloadChangedSincePreflight> {
    let Some(git_ref) = lab_offload_changed_since_ref(args) else {
        return Ok(LabOffloadChangedSincePreflight {
            args: args.to_vec(),
            requested_ref: None,
            resolved_base: None,
            git_fetch_refs: Vec::new(),
        });
    };

    let resolved_base = resolve_changed_since_base(source_path, &git_ref)?;
    ensure_local_merge_base(source_path, &git_ref)?;
    let git_fetch_refs = advertised_origin_ref_for_commit(source_path, &resolved_base)?
        .into_iter()
        .collect();

    Ok(LabOffloadChangedSincePreflight {
        args: rewrite_changed_since_ref(args, &resolved_base),
        requested_ref: Some(git_ref),
        resolved_base: Some(resolved_base),
        git_fetch_refs,
    })
}

pub fn lab_offload_changed_since_ref(args: &[String]) -> Option<String> {
    if lab_offload_has_precomputed_changed_files(args) {
        return None;
    }

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

fn lab_offload_has_precomputed_changed_files(args: &[String]) -> bool {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--lab-changed-files-json" {
            return iter.next().is_some();
        }
        if arg.starts_with("--lab-changed-files-json=") {
            return true;
        }
    }
    false
}

fn rewrite_changed_since_ref(args: &[String], resolved_base: &str) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;

    while let Some(arg) = iter.next() {
        if passthrough {
            rewritten.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            rewritten.push(arg.clone());
            continue;
        }
        if arg == "--changed-since" {
            rewritten.push(arg.clone());
            if iter.peek().is_some() {
                let _ = iter.next();
                rewritten.push(resolved_base.to_string());
            }
            continue;
        }
        if arg.starts_with("--changed-since=") {
            rewritten.push(format!("--changed-since={resolved_base}"));
            continue;
        }
        rewritten.push(arg.clone());
    }

    rewritten
}

fn resolve_changed_since_base(path: &Path, git_ref: &str) -> Result<String> {
    git_output(
        path,
        &["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")],
    )
}

fn advertised_origin_ref_for_commit(path: &Path, commit: &str) -> Result<Option<String>> {
    let refs = advertised_origin_refs_for_commit(
        path,
        commit,
        "changed_since",
        "Lab offload could not inspect origin refs for changed-since base materialization",
        commit.to_string(),
        vec!["Run with --force-hot to execute the changed-since command locally while investigating remote ref availability.".to_string()],
    )?;
    Ok(best_advertised_ref(refs))
}

fn ensure_local_merge_base(path: &Path, git_ref: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["merge-base", git_ref, "HEAD"])
        .current_dir(path)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("run git merge-base".to_string()))
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "changed_since",
        "Lab offload cannot resolve the requested --changed-since base before dispatch",
        Some(git_ref.to_string()),
        Some(vec![
            format!("Fetch or correct the base ref locally: git fetch origin {git_ref}"),
            "Run with --force-hot to execute the changed-since command locally.".to_string(),
        ]),
    ))
}

fn git_output(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some("run git".to_string())))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    Err(Error::validation_invalid_argument(
        "changed_since",
        "Lab offload cannot resolve the requested --changed-since base before dispatch",
        Some(args.last().copied().unwrap_or_default().to_string()),
        Some(vec![
            "Fetch the base ref locally before using Lab offload.".to_string(),
            "Run with --force-hot to execute the changed-since command locally.".to_string(),
        ]),
    ))
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
            "/Users/user/Developer/project".to_string(),
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
            "/Users/user/Developer/project".to_string(),
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
            "/Users/user/Developer/project".to_string(),
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
    fn precomputed_changed_files_make_changed_since_snapshot_portable() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/user/Developer/project".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
            "--lab-changed-files-json".to_string(),
            "[\"src/lib.rs\"]".to_string(),
        ];

        assert_eq!(lab_offload_changed_since_ref(&args), None);

        let preflight =
            preflight_lab_offload_changed_since(&args, RunnerWorkspaceSyncMode::Snapshot)
                .expect("precomputed changed-file scope should be snapshot portable");
        assert_eq!(preflight.args, args);
        assert_eq!(preflight.requested_ref, None);
        assert_eq!(preflight.resolved_base, None);
        assert!(preflight.git_fetch_refs.is_empty());
    }

    #[test]
    fn precomputed_changed_files_ignore_passthrough_payloads() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
            "--".to_string(),
            "--lab-changed-files-json".to_string(),
            "[\"fixture\"]".to_string(),
        ];

        assert_eq!(
            lab_offload_changed_since_ref(&args),
            Some("origin/main".to_string())
        );
    }

    #[test]
    fn allows_changed_since_for_git_lab_offload() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/user/Developer/project".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
        ];

        preflight_lab_offload_changed_since(&args, RunnerWorkspaceSyncMode::Git)
            .expect("git Lab offload can preserve changed-since semantics");
    }

    #[test]
    fn rewrites_changed_since_to_resolved_commit_for_git_lab_offload() {
        let dir = tempfile::tempdir().expect("repo");
        let origin = tempfile::tempdir().expect("origin");
        git(origin.path(), &["init", "--bare"]);
        git(dir.path(), &["init"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test User"]);
        git(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                origin.path().to_str().expect("origin path"),
            ],
        );
        std::fs::write(dir.path().join("file.txt"), "base\n").expect("write base");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "base"]);
        git(dir.path(), &["branch", "base"]);
        git(dir.path(), &["push", "origin", "base"]);
        std::fs::write(dir.path().join("file.txt"), "head\n").expect("write head");
        git(dir.path(), &["commit", "-am", "head"]);
        let base_sha = git_stdout(dir.path(), &["rev-parse", "base"]);

        let args = vec![
            "homeboy".to_string(),
            "audit".to_string(),
            "--changed-since".to_string(),
            "base".to_string(),
        ];

        let preflight = prepare_git_lab_offload_changed_since(&args, dir.path())
            .expect("changed-since preflight");

        assert_eq!(preflight.resolved_base.as_deref(), Some(base_sha.as_str()));
        assert_eq!(preflight.requested_ref.as_deref(), Some("base"));
        assert_eq!(preflight.git_fetch_refs, vec!["refs/heads/base"]);
        assert_eq!(
            preflight.args,
            vec![
                "homeboy".to_string(),
                "audit".to_string(),
                "--changed-since".to_string(),
                base_sha,
            ]
        );
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
