use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use crate::core::engine::shell;
use crate::core::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::materializer::{WorkspaceMaterializationOperation, WorkspaceMaterializer};
use super::types::GitSnapshot;
use super::util::{git_output, run_shell_command, ssh_args, ssh_client_for_runner};

pub(super) fn git_snapshot(
    local_path: &Path,
    changed_since_base: Option<&str>,
    git_fetch_refs: Vec<String>,
    controller_routed_git: bool,
) -> Result<GitSnapshot> {
    let head = git_output(local_path, &["rev-parse", "HEAD"])?;
    let branch = git_output(local_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|branch| branch != "HEAD");
    if !controller_routed_git {
        ensure_clean_git_working_tree(local_path, changed_since_base)?;
    }
    let remote_url = git_output(local_path, &["config", "--get", "remote.origin.url"])?;
    if remote_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "remote.origin.url",
            "git workspace sync requires remote.origin.url",
            None,
            None,
        ));
    }
    if controller_routed_git
        || branch.is_none()
        || super::super::source_materialization::requires_controller_routed_workspace_sync(
            &remote_url,
        )
    {
        let refs = controller_bundle_refs(&head, changed_since_base, &git_fetch_refs);
        repair_controller_bundle_commit_closure(local_path, &refs)?;
    }
    if controller_routed_git {
        ensure_clean_git_working_tree(local_path, changed_since_base)?;
    }

    Ok(GitSnapshot {
        remote_url,
        head,
        branch,
        changed_since_base: changed_since_base.map(str::to_string),
        git_fetch_refs,
    })
}

fn ensure_clean_git_working_tree(
    local_path: &Path,
    changed_since_base: Option<&str>,
) -> Result<()> {
    let status = git_output(local_path, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        if changed_since_base.is_some() {
            return Err(Error::validation_invalid_argument(
                "mode",
                "git workspace sync requires a clean working tree for changed-since remote execution; snapshot sync cannot honor --changed-since because it excludes .git metadata",
                Some("git".to_string()),
                Some(vec![
                    "Commit or stash local changes before remote execution of a --changed-since command."
                        .to_string(),
                    "Run with --placement local to execute the changed-since command locally."
                        .to_string(),
                    "Omit --changed-since to use snapshot remote execution for dirty local changes."
                        .to_string(),
                ]),
            ));
        }

        return Err(Error::validation_invalid_argument(
            "mode",
            "git workspace sync requires a clean working tree before remote execution",
            Some("git".to_string()),
            Some(vec![
                "Commit or stash local changes before git-backed Lab execution.".to_string(),
                "Run with --placement local to execute the command locally while the worktree is dirty."
                    .to_string(),
                "Use `homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot` when materializing a standalone snapshot workspace."
                    .to_string(),
            ]),
        ));
    }
    Ok(())
}

pub(super) fn materialize_git(
    runner: &Runner,
    remote_path: &str,
    remote_url: &str,
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
    allow_dirty_lab_workspace: bool,
) -> Result<()> {
    let command = materialize_git_command(
        remote_path,
        remote_url,
        head,
        changed_since_base,
        git_fetch_refs,
        allow_dirty_lab_workspace,
    );
    match runner.kind {
        RunnerKind::Local => run_shell_command(&command, "materialize local git workspace"),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            let output = client.execute(&command);
            if output.success {
                Ok(())
            } else {
                Err(Error::validation_invalid_argument(
                    "changed_since",
                    "runner dispatch could not make the requested --changed-since base reachable in the runner workspace before dispatch",
                    changed_since_base.map(str::to_string),
                    Some(vec![
                        "Verify the branch and base commit are pushed to origin.".to_string(),
                        "Run with --placement local to execute the changed-since command locally."
                            .to_string(),
                        format!("Remote git error: {}", output.stderr.trim()),
                    ]),
                ))
            }
        }
    }
}

pub(super) fn materialize_git_from_controller_bundle(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    head: &str,
    branch: Option<&str>,
    remote_url: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
    allow_dirty_lab_workspace: bool,
) -> Result<()> {
    validate_controller_git_bundle_source(local_path)?;

    let bundle_dir = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create controller git bundle directory".to_string()),
        )
    })?;
    let bundle_path = bundle_dir.path().join("workspace.bundle");

    let refs = controller_bundle_refs("HEAD", changed_since_base, git_fetch_refs);
    hydrate_controller_bundle_objects(local_path, &refs)?;

    let output = Command::new("git")
        .arg("bundle")
        .arg("create")
        .arg(&bundle_path)
        .args(&refs)
        .current_dir(local_path)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("create git bundle".to_string()))
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "create git bundle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let install_command = git_bundle_install_command(
        remote_path,
        head,
        branch,
        remote_url,
        allow_dirty_lab_workspace,
    );
    let result = match runner.kind {
        RunnerKind::Local => materialize_git_bundle_piped(
            &bundle_path,
            &format!("sh -c {}", shell::quote_arg(&install_command)),
            "materialize local git bundle workspace",
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                materialize_git_bundle_piped(
                    &bundle_path,
                    &format!("sh -c {}", shell::quote_arg(&install_command)),
                    "materialize local git bundle workspace",
                )
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let target = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&install_command),
                );
                materialize_git_bundle_piped(
                    &bundle_path,
                    &target,
                    "materialize SSH git bundle workspace",
                )
            }
        }
    };

    result
}

/// Resolve the exact object closure locally before `git bundle` can invoke a
/// promisor remote itself. The controller is the only participant allowed to
/// use the source checkout's authenticated transport.
fn hydrate_controller_bundle_objects(local_path: &Path, refs: &[String]) -> Result<()> {
    let mut object_list = Command::new("git")
        // A stale commit graph can name promisor objects that do not exist in
        // the local database. Walk the repaired closure from real objects so
        // Git can use the controller's promisor transport when needed.
        .args([
            "-c",
            "core.commitGraph=false",
            "rev-list",
            "--objects",
            "--no-object-names",
        ])
        .args(refs)
        .current_dir(local_path)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("list git bundle objects".to_string()))
        })?;
    let mut hydrate = Command::new("git")
        .args(["cat-file", "--batch-check"])
        .current_dir(local_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("hydrate git bundle objects".to_string()),
            )
        })?;

    let mut object_list_stdout = object_list
        .stdout
        .take()
        .expect("piped git object list stdout");
    let mut hydrate_stdin = hydrate
        .stdin
        .take()
        .expect("piped git object hydration stdin");
    std::io::copy(&mut object_list_stdout, &mut hydrate_stdin).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("stream git bundle objects".to_string()),
        )
    })?;
    drop(hydrate_stdin);

    let object_list_status = object_list.wait().map_err(|err| {
        Error::internal_io(err.to_string(), Some("list git bundle objects".to_string()))
    })?;
    if !object_list_status.success() {
        return Err(Error::internal_unexpected(
            "list git bundle objects failed while resolving the controller object closure",
        ));
    }
    let hydrate_status = hydrate.wait().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("hydrate git bundle objects".to_string()),
        )
    })?;
    if !hydrate_status.success() {
        return Err(Error::internal_unexpected(
            "hydrate git bundle objects failed while resolving the controller object closure",
        ));
    }

    Ok(())
}

fn repair_controller_bundle_commit_closure(local_path: &Path, refs: &[String]) -> Result<()> {
    if let Some(remote) = promisor_remote(local_path)? {
        refetch_controller_bundle_commits(local_path, &remote, refs)?;
    }
    Ok(())
}

/// Rebuild the selected commit and tree closure before asking Git to walk it.
/// A commit graph can retain entries for promisor objects that are no longer in
/// the local object database, in which case `rev-list` cannot trigger its own
/// lazy fetch.
fn refetch_controller_bundle_commits(
    local_path: &Path,
    remote: &str,
    refs: &[String],
) -> Result<()> {
    let output = Command::new("git")
        .args(["fetch", "--refetch", "--no-tags", "--filter=blob:none"])
        .arg(remote)
        .args(refs)
        .current_dir(local_path)
        .output()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("refetch controller git bundle commits".to_string()),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(Error::internal_unexpected(format!(
        "refetch controller git bundle commits failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )))
}

fn promisor_remote(local_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", "--get-regexp", r"^remote\..*\.promisor$"])
        .current_dir(local_path)
        .output()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read git promisor remote".to_string()),
            )
        })?;
    if !output.status.success() {
        return Ok(None);
    }

    let remote = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(char::is_whitespace)?;
            if value.trim() != "true" {
                return None;
            }
            key.strip_prefix("remote.")?.strip_suffix(".promisor")
        })
        .map(str::to_string);
    Ok(remote)
}

fn controller_bundle_refs(
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
) -> Vec<String> {
    let mut refs = vec![head.to_string()];
    if let Some(base) = changed_since_base {
        push_unique_bundle_ref(&mut refs, base);
    }
    for git_ref in git_fetch_refs {
        // A fetch refspec may name a destination for runner-side fetches. A
        // bundle only needs its controller-local source ref.
        let source_ref = git_ref
            .trim_start_matches('+')
            .split_once(':')
            .map_or(git_ref.as_str(), |(source, _)| source);
        push_unique_bundle_ref(&mut refs, source_ref);
    }
    refs
}

fn push_unique_bundle_ref(refs: &mut Vec<String>, git_ref: &str) {
    if !git_ref.trim().is_empty() && !refs.iter().any(|existing| existing == git_ref) {
        refs.push(git_ref.to_string());
    }
}

fn validate_controller_git_bundle_source(local_path: &Path) -> Result<()> {
    let is_shallow = git_output(local_path, &["rev-parse", "--is-shallow-repository"])?;
    if is_shallow.trim() != "true" {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "path",
        "controller-routed git workspace sync requires a full source checkout before creating a runner git bundle; the selected source checkout is shallow",
        Some(local_path.display().to_string()),
        Some(vec![
            format!(
                "Deepen the source checkout with `git -C {} fetch --unshallow` before retrying.",
                shell::quote_arg(&local_path.display().to_string())
            ),
            "Use a full clone for --source-path when upgrading runners with --method source."
                .to_string(),
            "Use snapshot workspace sync only when the remote command does not need Git history."
                .to_string(),
        ]),
    ))
}

fn materialize_git_bundle_piped(
    bundle_path: &Path,
    target_command: &str,
    action: &str,
) -> Result<()> {
    let command = format!(
        "cat {bundle} | {target_command}",
        bundle = shell::quote_arg(&bundle_path.display().to_string()),
        target_command = target_command,
    );
    run_shell_command(&command, action)
}

pub(crate) fn git_bundle_install_command(
    remote_path: &str,
    head: &str,
    branch: Option<&str>,
    remote_url: &str,
    allow_dirty_lab_workspace: bool,
) -> String {
    WorkspaceMaterializer::new(remote_path)
        .with_bundle_file()
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::CleanupOnExit(vec![
            "\"$tmp\"".to_string(),
            "\"$bundle\"".to_string(),
        ]))
        .op(WorkspaceMaterializationOperation::WriteStdinToBundle)
        .op(WorkspaceMaterializationOperation::CloneBundleToTemp)
        .op(WorkspaceMaterializationOperation::SetGitOrigin(
            remote_url.to_string(),
        ))
        .op(WorkspaceMaterializationOperation::CheckoutGitRef {
            head: head.to_string(),
            branch: branch.map(str::to_string),
        })
        .op(WorkspaceMaterializationOperation::ResetAndCleanGit {
            head: head.to_string(),
        })
        .op(WorkspaceMaterializationOperation::GuardCleanGitWorkspace {
            allow_dirty: allow_dirty_lab_workspace,
        })
        .op(WorkspaceMaterializationOperation::AtomicReplaceTemp)
        .restore_owner()
        .command()
}

pub(super) fn materialize_git_command(
    remote_path: &str,
    remote_url: &str,
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
    allow_dirty_lab_workspace: bool,
) -> String {
    WorkspaceMaterializer::new(remote_path)
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::SyncGitCheckout {
            remote_url: remote_url.to_string(),
            head: head.to_string(),
            changed_since_base: changed_since_base.map(str::to_string),
            fetch_refs: git_fetch_refs.to_vec(),
            allow_dirty: allow_dirty_lab_workspace,
        })
        .restore_owner()
        .command()
}
