use super::super::helpers::current_version;
use super::super::types::InstallMethod;
use super::*;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerCapabilityPreflight;
use crate::core::runner::RunnerExecOptions;
use crate::core::runner::RunnerKind;
use crate::core::runner::RunnerRequiredTool;
use crate::core::runner::RunnerWorkspaceSyncMode;
use crate::core::runner::RunnerWorkspaceSyncOptions;
use crate::core::Result;
use std::path::Path;
use std::process::Command;

pub fn source_checkout_build_identity(source_path: &Path) -> Option<String> {
    // `rev-parse` must produce a commit hash; an empty result means we can't
    // identify the checkout, so treat it as failure.
    let commit = git_output(source_path, &["rev-parse", "--short=12", "HEAD"])?;
    if commit.trim().is_empty() {
        return None;
    }
    // `git status --porcelain` returns empty output for a clean tree, which is
    // the normal, successful case — empty here must NOT be treated as failure.
    let status = git_output(source_path, &["status", "--porcelain"])?;
    let dirty_suffix = if status.trim().is_empty() {
        ""
    } else {
        "-dirty"
    };

    Some(format!(
        "homeboy {}+{}{}",
        current_version(),
        commit.trim(),
        dirty_suffix
    ))
}

pub fn git_output(source_path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source_path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    // Return the (possibly empty) stdout on success. Empty output is valid for
    // commands like `git status --porcelain` on a clean tree; callers that
    // require non-empty output must validate it themselves.
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn prepare_runner_source_checkout_for_upgrade(
    runner: &Runner,
    method_override: Option<InstallMethod>,
    source_path: Option<&str>,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Option<String> {
    if method_override != Some(InstallMethod::Source) || runner.kind != RunnerKind::Ssh {
        return None;
    }

    let source_path = source_path?;
    let command = vec![
        "sh".to_string(),
        "-lc".to_string(),
        runner_source_checkout_prepare_script().to_string(),
    ];
    let mut options = runner_source_checkout_prepare_options(runner, command);
    options.cwd = Some(source_path.to_string());

    match exec(&runner.id, options) {
        Ok((_, 0)) => None,
        Ok((output, exit_code)) => Some(format!(
            "runner source checkout preparation failed with exit code {exit_code}: {}",
            runner_upgrade_detail(&output)
        )),
        Err(err) => Some(format!(
            "runner source checkout preparation failed: {}",
            err.message
        )),
    }
}

pub fn runner_source_checkout_prepare_options(
    runner: &Runner,
    command: Vec<String>,
) -> RunnerExecOptions {
    RunnerExecOptions {
        cwd: None,
        project_id: None,
        allow_diagnostic_ssh: true,
        command,
        env: runner.env.clone(),
        secret_env_names: Vec::new(),
        capture_patch: false,
        raw_exec: false,
        source_snapshot: None,
        capability_preflight: Some(RunnerCapabilityPreflight {
            command: "prepare source checkout for homeboy upgrade".to_string(),
            required_tools: vec![RunnerRequiredTool::Git],
            required_commands: Vec::new(),
            required_components: Vec::new(),
            required_env: Vec::new(),
        }),
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
        detach_after_handoff: false,
    }
}

pub fn runner_source_checkout_prepare_script() -> &'static str {
    r#"set -e
git fetch origin
if git symbolic-ref -q HEAD >/dev/null && git rev-parse --abbrev-ref --symbolic-full-name @{upstream} >/dev/null 2>&1; then
  git pull --ff-only
  exit 0
fi
remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD || true)"
if [ -z "$remote_head" ]; then
  echo "Cannot determine origin default branch for source checkout" >&2
  exit 1
fi
git checkout --detach "$remote_head"
"#
}

pub fn runner_upgrade_source_path(
    runner: &Runner,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    materialize_source_path: &mut impl FnMut(&Runner, &Path) -> Result<String>,
) -> Result<Option<String>> {
    let Some(source_path) = source_path else {
        return Ok(None);
    };

    if method_override == Some(InstallMethod::Source) && runner.kind == RunnerKind::Ssh {
        return materialize_source_path(runner, source_path).map(Some);
    }

    Ok(Some(source_path.display().to_string()))
}

pub fn materialize_runner_source_path(runner: &Runner, source_path: &Path) -> Result<String> {
    let (output, _) = runner::sync_workspace(
        &runner.id,
        RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            mode: RunnerWorkspaceSyncMode::Git,
            controller_routed_git: true,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?;

    Ok(output.remote_path)
}
