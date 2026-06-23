use std::path::Path;
use std::process::Command;

use crate::core::engine::shell;
use crate::core::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::types::GitSnapshot;
use super::util::{
    git_output, owner_capture_shell, owner_restore_shell, parent_remote_path, run_shell_command,
    ssh_args, ssh_client_for_runner,
};

pub(super) fn git_snapshot(
    local_path: &Path,
    changed_since_base: Option<&str>,
    git_fetch_refs: Vec<String>,
) -> Result<GitSnapshot> {
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
                    "Run with --force-hot to execute the changed-since command locally."
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
                "Run with --force-hot to execute the command locally while the worktree is dirty."
                    .to_string(),
                "Use `homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot` when materializing a standalone snapshot workspace."
                    .to_string(),
            ]),
        ));
    }
    let head = git_output(local_path, &["rev-parse", "HEAD"])?;
    let branch = git_output(local_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|branch| branch != "HEAD");
    let remote_url = git_output(local_path, &["config", "--get", "remote.origin.url"])?;
    if remote_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "remote.origin.url",
            "git workspace sync requires remote.origin.url",
            None,
            None,
        ));
    }
    Ok(GitSnapshot {
        remote_url,
        head,
        branch,
        changed_since_base: changed_since_base.map(str::to_string),
        git_fetch_refs,
    })
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
                        "Run with --force-hot to execute the changed-since command locally."
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

    let mut refs = vec![
        head.to_string(),
        "--branches".to_string(),
        "--tags".to_string(),
    ];
    if let Some(base) = changed_since_base {
        refs.push(base.to_string());
    }
    refs.extend(git_fetch_refs.iter().cloned());

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

fn git_bundle_install_command(
    remote_path: &str,
    head: &str,
    branch: Option<&str>,
    remote_url: &str,
    allow_dirty_lab_workspace: bool,
) -> String {
    let parent = parent_remote_path(remote_path);
    let checkout = if let Some(branch) = branch {
        format!(
            "git -C \"$tmp\" checkout -B {branch} {head} && git -C \"$tmp\" config branch.{branch}.remote origin && git -C \"$tmp\" config branch.{branch}.merge refs/heads/{branch}",
            branch = shell::quote_arg(branch),
            head = shell::quote_arg(head),
        )
    } else {
        format!(
            "git -C \"$tmp\" checkout --detach {head}",
            head = shell::quote_arg(head)
        )
    };

    let dirty_guard = dirty_lab_workspace_guard("$dest", allow_dirty_lab_workspace);
    format!(
        "parent={parent}; dest={dest}; tmp=\"${{dest}}.tmp.$$\"; bundle=\"${{dest}}.bundle.$$\"; {owner_capture}; mkdir -p \"$parent\" && trap 'rm -rf \"$tmp\" \"$bundle\"' EXIT; rm -rf \"$tmp\" \"$bundle\" && cat > \"$bundle\" && git clone \"$bundle\" \"$tmp\" && git -C \"$tmp\" remote set-url origin {remote_url} && {checkout} && git -C \"$tmp\" reset --hard {head} && git -C \"$tmp\" clean -ffdqx && {dirty_guard} && rm -rf \"$dest\" && mv \"$tmp\" \"$dest\" && {owner_restore}",
        parent = shell::quote_arg(parent.as_str()),
        dest = shell::quote_arg(remote_path),
        remote_url = shell::quote_arg(remote_url),
        checkout = checkout,
        head = shell::quote_arg(head),
        dirty_guard = dirty_guard,
        owner_capture = owner_capture_shell("$parent"),
        owner_restore = owner_restore_shell("$parent", "$dest"),
    )
}

pub(super) fn materialize_git_command(
    remote_path: &str,
    remote_url: &str,
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
    allow_dirty_lab_workspace: bool,
) -> String {
    let parent = parent_remote_path(remote_path);
    let dest = shell::quote_arg(remote_path);
    let fetch_changed_since = changed_since_base
        .map(|base| {
            format!(
                " && (git -C {dest} rev-parse --verify -q {} >/dev/null || git -C {dest} fetch origin {})",
                shell::quote_arg(&format!("{base}^{{commit}}")),
                shell::quote_arg(base)
            )
        })
        .unwrap_or_default();
    let fetch_extra_refs = git_fetch_refs
        .iter()
        .map(|git_ref| {
            format!(
                " && git -C {dest} fetch origin {}",
                shell::quote_arg(git_ref)
            )
        })
        .collect::<String>();

    let dirty_guard = dirty_lab_workspace_guard("$dest", allow_dirty_lab_workspace);

    format!(
        "parent={parent}; dest={dest}; {owner_capture}; mkdir -p \"$parent\" && if [ -d \"$dest\"/.git ]; then {dirty_guard} && git -C \"$dest\" reset --hard && git -C \"$dest\" clean -ffdqx && git -C \"$dest\" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; else rm -rf \"$dest\" && git clone {url} \"$dest\" && git -C \"$dest\" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; fi{fetch_extra_refs}{fetch_changed_since} && git -C \"$dest\" checkout --detach {head} && git -C \"$dest\" reset --hard {head} && git -C \"$dest\" clean -ffdqx && {owner_restore}",
        parent = shell::quote_arg(parent.as_str()),
        dest = dest,
        url = shell::quote_arg(remote_url),
        head = shell::quote_arg(head),
        fetch_changed_since = fetch_changed_since,
        fetch_extra_refs = fetch_extra_refs,
        dirty_guard = dirty_guard,
        owner_capture = owner_capture_shell("$parent"),
        owner_restore = owner_restore_shell("$parent", "$dest"),
    )
}

fn dirty_lab_workspace_guard(dest: &str, allow_dirty_lab_workspace: bool) -> String {
    let status = format!(
        "git -C {dest} status --porcelain=v1 2>/dev/null | while IFS= read -r line; do path=${{line#???}}; if [ \"$path\" = .homeboy ] || [ \"${{path#.homeboy/}}\" != \"$path\" ]; then :; else printf '%s\\n' \"$line\"; fi; done || true",
        dest = dest,
    );
    if allow_dirty_lab_workspace {
        format!(
            "dirty=$({status}); if [ -n \"$dirty\" ]; then printf '%s\\n' 'Homeboy Lab warning: --allow-dirty-lab-workspace is overwriting uncommitted runner workspace changes.' >&2; printf '%s\\n' \"$dirty\" >&2; fi",
            status = status,
        )
    } else {
        format!(
            "dirty=$({status}); if [ -n \"$dirty\" ]; then printf '%s\\n' 'Homeboy Lab refused to overwrite a dirty runner workspace.' >&2; printf '%s\\n' \"$dirty\" >&2; printf '%s\\n' 'Commit, stash, clean, or remove the runner workspace before retrying. Pass --allow-dirty-lab-workspace only for noisy investigation that may discard runner-side changes.' >&2; exit 97; fi",
            status = status,
        )
    }
}
