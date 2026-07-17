use std::path::{Path, PathBuf};

use super::*;

pub(super) fn run_rig_source_management_on_runner(
    runner_id: &str,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<(String, String, i32)> {
    let runner = runners::load(runner_id)?;
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let mut command = runner_rig_source_management_command(homeboy_path, normalized_args);
    let rig_install_source_root = rig_install_source_sync_root(&command);
    let mut translated_remote_root = runner.workspace_root.clone().unwrap_or_default();
    let local_cwd = std::env::current_dir().ok();
    if let Some(local_cwd) = local_cwd.as_deref() {
        if command_contains_path_prefix(&command, local_cwd) {
            let (synced, sync_exit_code) = runners::sync_workspace(
                runner_id,
                runners::RunnerWorkspaceSyncOptions {
                    path: local_cwd.display().to_string(),
                    mode: runners::RunnerWorkspaceSyncMode::Snapshot,
                    ..Default::default()
                },
            )?;
            if sync_exit_code == 0 {
                command = translate_command_path_prefix(&command, local_cwd, &synced.remote_path);
                translated_remote_root = synced.remote_path;
            }
        }
    }

    // `rig install <source>` forwards the controller-local rig package path. When
    // that package lives outside the synced working directory the runner cannot
    // see it and fails with `Path does not exist: /Users/...`, so the documented
    // one-command `rig install --runner` offload is broken without a local
    // pre-install + `--skip-install` (#6964). Materialize the source's containing
    // checkout on the runner and translate the forwarded path through the same
    // sync+translate seam the working directory already uses. Syncing the
    // containing checkout (not just the package directory) keeps rigs that
    // declare `package_dependencies` — which resolve against the repo root — and
    // package-level `extends` templates working on the runner.
    if let Some(source_root) = rig_install_source_root.as_deref() {
        let (synced, sync_exit_code) = runners::sync_workspace(
            runner_id,
            runners::RunnerWorkspaceSyncOptions {
                path: source_root.display().to_string(),
                mode: runners::RunnerWorkspaceSyncMode::Snapshot,
                ..Default::default()
            },
        )?;
        if sync_exit_code == 0 {
            command = translate_command_path_prefix(&command, source_root, &synced.remote_path);
            translated_remote_root = synced.remote_path;
        }
    }

    command = strip_rig_source_management_local_wrapper_flags(&command);
    if let Some(source_root) = rig_install_source_root.as_deref() {
        runners::preflight_remote_argv_path_translation(
            "Rig source management",
            runner_id,
            &command,
            source_root,
            &translated_remote_root,
        )?;
    } else if let Some(local_cwd) = local_cwd.as_deref() {
        runners::preflight_remote_argv_path_translation(
            "Rig source management",
            runner_id,
            &command,
            local_cwd,
            &translated_remote_root,
        )?;
    }

    let (output, exit_code) = runners::exec(
        runner_id,
        RunnerExecOptions {
            cwd: runner.workspace_root.clone(),
            project_id: None,
            allow_diagnostic_ssh: false,
            diagnostic_ssh_timeout: None,
            command,
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            env_materialization: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            path_materialization_plan: None,
            capability_preflight: Some(runners::RunnerCapabilityPreflight {
                command: "rig_source_management".to_string(),
                required_commands: vec![homeboy_path.to_string()],
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            accepted_extension_settings: Vec::new(),
            require_paths: Vec::new(),
            lab_runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;

    if let Some(path) = output_file {
        write_offloaded_stdout(path, &output.stdout)?;
    }

    Ok((output.stdout, output.stderr, exit_code))
}

pub(super) fn command_contains_path_prefix(command: &[String], local_root: &Path) -> bool {
    let local_root = local_root.display().to_string();
    let local_root = local_root.trim_end_matches('/');
    !local_root.is_empty() && command.iter().any(|arg| arg.contains(local_root))
}

pub(super) fn translate_command_path_prefix(
    command: &[String],
    local_root: &Path,
    remote_root: &str,
) -> Vec<String> {
    let local_root = local_root.display().to_string();
    let local_root = local_root.trim_end_matches('/');
    let remote_root = remote_root.trim_end_matches('/');
    command
        .iter()
        .map(|arg| {
            if local_root.is_empty() || remote_root.is_empty() {
                arg.clone()
            } else {
                arg.replace(local_root, remote_root)
            }
        })
        .collect()
}

/// Resolve the controller-local sync root for a `rig install <source>` argument
/// that still references a path on this controller after working-directory
/// translation. Returns the source's containing git checkout (or the path
/// itself when it is not inside a git repo) so the caller can materialize it on
/// the runner and translate the forwarded argument (#6964).
///
/// Returns `None` for git-URL sources, paths already translated to the runner,
/// and any source that does not exist on this controller — leaving non-path and
/// already-remote arguments untouched.
pub(super) fn rig_install_source_sync_root(command: &[String]) -> Option<PathBuf> {
    let source = rig_install_source_arg(command)?;
    let expanded = shellexpand::tilde(&source).to_string();
    let path = Path::new(&expanded);
    if !path.exists() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    Some(homeboy::core::git::repo_root(&canonical).unwrap_or(canonical))
}

/// Return the positional `<source>` of a `rig install` command, skipping the
/// flags the subcommand accepts (`--id <value>`, `--id=<value>`, `--all`,
/// `--reinstall`/`--force`). The command argv is `[homeboy, rig, install, ...]`
/// with controller-only globals already stripped by
/// [`runner_rig_source_management_command`].
pub(super) fn rig_install_source_arg(command: &[String]) -> Option<String> {
    let install_index = command
        .windows(2)
        .position(|window| window[0] == "rig" && window[1] == "install")?
        + 2;
    let mut iter = command[install_index..].iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return iter.next().cloned();
        }
        if arg == "--id" {
            iter.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.clone());
    }
    None
}

pub(super) fn runner_rig_source_management_command(
    homeboy_path: &str,
    normalized_args: &[String],
) -> Vec<String> {
    let mut command = vec![homeboy_path.to_string()];
    command.extend(normalized_args.iter().skip(1).cloned());
    command
}

pub(super) fn strip_rig_source_management_local_wrapper_flags(command: &[String]) -> Vec<String> {
    const FLAGS: [(&str, bool); 7] = [
        ("--runner", true),
        ("--output", true),
        ("--artifact-root", true),
        ("--placement", true),
        ("--allow-local-fallback", false),
        ("--allow-dirty-lab-workspace", false),
        ("--detach-after-handoff", false),
    ];

    let mut stripped = Vec::with_capacity(command.len());
    let mut args = command.iter().peekable();
    while let Some(arg) = args.next() {
        if let Some((_, takes_value)) = FLAGS
            .iter()
            .find(|(flag, _)| arg == flag || arg.starts_with(&format!("{flag}=")))
        {
            if *takes_value && !arg.contains('=') {
                args.next();
            }
            continue;
        }
        stripped.push(arg.clone());
    }
    stripped
}
