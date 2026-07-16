use super::super::helpers::current_version;
use super::super::types::InstallMethod;
use super::*;
use crate::runner;
use crate::runner::Runner;
use crate::runner::RunnerCapabilityPreflight;
use crate::runner::RunnerExecOptions;
use crate::runner::RunnerWorkspaceSyncMode;
use crate::runner::RunnerWorkspaceSyncOptions;
use crate::Result;
use homeboy_runner_contract::RunnerKind;
use homeboy_runner_contract::RunnerRequiredTool;
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
        Ok((output, exit_code)) => Some(source_checkout_prepare_failure_detail(
            runner,
            format!(
                "runner source checkout preparation failed with exit code {exit_code}: {}",
                runner_upgrade_detail(&output)
            ),
        )),
        Err(err) => Some(source_checkout_prepare_failure_detail(
            runner,
            format!("runner source checkout preparation failed: {}", err.message),
        )),
    }
}

fn source_checkout_prepare_failure_detail(runner: &Runner, detail: String) -> String {
    match runner.workspace_root.as_deref() {
        Some(workspace_root) => format!(
            "{detail}\nretry with clean managed runner source: {}",
            refresh_homeboy_command(runner, workspace_root)
        ),
        None => detail,
    }
}

fn refresh_homeboy_command(runner: &Runner, workspace_root: &str) -> String {
    let target_dir = format!(
        "{}/_homeboy_binaries/homeboy-main",
        workspace_root.trim_end_matches('/')
    );
    format!(
        "homeboy runner refresh-homeboy {} --ref main --target-dir {} --reconnect",
        shell_arg(&runner.id),
        shell_arg(&target_dir)
    )
}

pub fn runner_source_checkout_prepare_options(
    runner: &Runner,
    command: Vec<String>,
) -> RunnerExecOptions {
    RunnerExecOptions {
        cwd: None,
        project_id: None,
        allow_diagnostic_ssh: true,
        diagnostic_ssh_timeout: None,
        command,
        env: runner.env.clone(),
        secret_env_names: Vec::new(),
        secret_env_plan: None,
        env_materialization: None,
        capture_patch: false,
        raw_exec: false,
        source_snapshot: None,
        path_materialization_plan: None,
        capability_preflight: Some(RunnerCapabilityPreflight {
            command: "prepare source checkout for homeboy upgrade".to_string(),
            required_tools: vec![RunnerRequiredTool::git()],
            required_commands: Vec::new(),
            required_tool_capabilities: Vec::new(),
            required_components: Vec::new(),
            required_env: Vec::new(),
            timeout: None,
        }),
        required_extensions: Vec::new(),
        accepted_extension_settings: Vec::new(),
        require_paths: Vec::new(),
        runner_workload: None,
        run_id: None,
        detach_after_handoff: false,
        mirror_evidence: true,
        print_handoff: true,
    }
}

pub fn runner_source_checkout_prepare_script() -> &'static str {
    r#"set -e
# Source sync already materializes the controller-selected commit. Do not fetch,
# pull, or rewrite it: the source may be detached or on a local-only branch.
git rev-parse --verify HEAD >/dev/null
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

pub fn materialize_explicit_runner_source_path(
    runner: &Runner,
    source_path: &Path,
) -> Result<String> {
    let (output, _) = runner::sync_workspace(
        &runner.id,
        RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            // An explicit source path is a selected build input, not a remote
            // branch to refresh. SnapshotGit retains a local Git identity for
            // the source build without fetching or resetting either checkout.
            mode: RunnerWorkspaceSyncMode::SnapshotGit,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?;

    Ok(output.remote_path)
}
