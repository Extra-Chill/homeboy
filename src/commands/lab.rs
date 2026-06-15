use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::{Path, PathBuf};

use super::{CmdResult, GlobalArgs};
use homeboy::core::runners::{
    self as runner, runner_exec_failure_error, RunnerExecOptions, RunnerExecOutput,
    RunnerRequiredTool, RunnerStatusReport, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use homeboy::core::source_snapshot::SourceSnapshot;
use homeboy::core::Error;
use serde_json::Value;

#[derive(Args)]
pub struct LabArgs {
    #[command(subcommand)]
    command: Option<LabCommand>,
}

#[derive(Subcommand)]
enum LabCommand {
    /// Show Lab routing status and benchmark commands
    Status {
        /// Runner ID to inspect. Defaults to lab.preferred_runner or inferred default Lab runner.
        #[arg(long)]
        runner: Option<String>,
    },
    /// Print the runner-backed benchmark command for the provided bench args
    Bench {
        /// Arguments to pass after `homeboy bench`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Sync an extension install on a Lab runner by source and git ref
    ExtensionSync {
        /// Runner ID. Defaults to lab.preferred_runner or the only configured SSH Lab runner.
        #[arg(long)]
        runner: Option<String>,
        /// Git URL or runner-local path to the extension source
        #[arg(long)]
        source: String,
        /// Installed extension id to create or replace on the runner
        #[arg(long)]
        id: String,
        /// Git ref to check out for URL installs (branch, tag, or commit)
        #[arg(long = "ref")]
        revision: String,
        /// Install without replacing an existing runner extension
        #[arg(long)]
        no_replace: bool,
    },
}

#[derive(Serialize)]
pub struct LabOutput {
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_runner: Option<LabSelectedRunnerOutput>,
    config_key: &'static str,
    config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_workspace: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    managed_followups: Vec<LabFollowup>,
    guidance: Vec<String>,
}

#[derive(Serialize)]
pub struct LabFollowup {
    label: &'static str,
    command: String,
    purpose: &'static str,
}

#[derive(Serialize)]
pub struct LabSelectedRunnerOutput {
    runner_id: String,
    kind: String,
    configured_executable: String,
    daemon_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_root: Option<String>,
    readiness_state: String,
    connected: bool,
    status: RunnerStatusReport,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum LabCommandOutput {
    Status(LabOutput),
    ExtensionSync(LabExtensionSyncOutput),
}

#[derive(Serialize)]
pub struct LabExtensionSyncOutput {
    command: &'static str,
    runner_id: String,
    runner_homeboy_path: String,
    extension_id: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_source: Option<String>,
    source_revision: String,
    replace: bool,
    install_command: Vec<String>,
    execution: RunnerExecOutput,
}

pub fn run(args: LabArgs, _global: &GlobalArgs) -> CmdResult<LabCommandOutput> {
    let preferred_runner = homeboy::core::runners::resolve_default_lab_runner()?;
    let config_path = homeboy::core::defaults::config_path()?;
    let current_workspace = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string());
    match args.command.unwrap_or(LabCommand::Status { runner: None }) {
        LabCommand::Status { runner } => {
            let selected_runner = selected_lab_runner_status(runner.as_deref())?;
            let followup_runner = runner.as_deref().or(preferred_runner.as_deref());
            let managed_followups = lab_followups(followup_runner, current_workspace.as_deref());
            Ok((
                LabCommandOutput::Status(LabOutput {
                    command: "lab.status",
                    preferred_runner,
                    selected_runner,
                    config_key: "/lab/preferred_runner",
                    config_path,
                    current_workspace,
                    managed_followups,
                    guidance: vec![
                        "Use `homeboy bench <component>` to run benchmarks on the default Lab runner."
                            .to_string(),
                        "Use the `managed_followups` commands when a Lab run needs runner diagnostics, environment inspection, workspace materialization, or managed execution.".to_string(),
                        "Use `homeboy config set /lab/preferred_runner '\"<runner-id>\"'` to set the default Lab runner.".to_string(),
                        "Use `homeboy config set /bench/local_execution '\"denied\"'` to make local benchmark execution fail closed.".to_string(),
                        "Use `--runner <runner-id>` only when multiple Lab runners are available and no default should be inferred.".to_string(),
                    ],
                }),
                0,
            ))
        }
        LabCommand::Bench { args } => {
            let managed_followups =
                lab_followups(preferred_runner.as_deref(), current_workspace.as_deref());
            let mut bench_command = "homeboy bench".to_string();
            if !args.is_empty() {
                bench_command.push(' ');
                bench_command.push_str(&args.join(" "));
            }
            Ok((
                LabCommandOutput::Status(LabOutput {
                    command: "lab.bench",
                    preferred_runner,
                    selected_runner: None,
                    config_key: "/lab/preferred_runner",
                    config_path,
                    current_workspace,
                    managed_followups,
                    guidance: vec![
                        bench_command,
                        "Homeboy auto-routes portable benchmarks to `lab.preferred_runner`, or to the only configured SSH Lab runner when there is exactly one.".to_string(),
                        "Use `--runner <runner-id>` only to override an ambiguous or non-default Lab selection.".to_string(),
                    ],
                }),
                0,
            ))
        }
        LabCommand::ExtensionSync {
            runner,
            source,
            id,
            revision,
            no_replace,
        } => sync_lab_extension(runner, &source, &id, &revision, !no_replace),
    }
}

fn selected_lab_runner_status(
    runner_id: Option<&str>,
) -> homeboy::core::Result<Option<LabSelectedRunnerOutput>> {
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };
    let runner_config = runner::load(runner_id)?;
    let status = runner::status(runner_id)?;
    Ok(Some(LabSelectedRunnerOutput {
        runner_id: runner_id.to_string(),
        kind: format!("{:?}", runner_config.kind).to_ascii_lowercase(),
        configured_executable: runner_config
            .settings
            .homeboy_path
            .clone()
            .unwrap_or_else(|| "homeboy".to_string()),
        daemon_enabled: runner_config.settings.daemon,
        workspace_root: runner_config.workspace_root.clone(),
        readiness_state: format!("{:?}", status.state).to_ascii_lowercase(),
        connected: status.connected,
        status,
    }))
}

fn sync_lab_extension(
    runner_id: Option<String>,
    source: &str,
    extension_id: &str,
    revision: &str,
    replace: bool,
) -> CmdResult<LabCommandOutput> {
    let runner_id = match runner_id {
        Some(runner_id) => runner_id,
        None => homeboy::core::runners::resolve_default_lab_runner()?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "No default Lab runner is configured or inferable",
                None,
                Some(vec![
                    "Pass --runner <runner-id>.".to_string(),
                    "Set one with: homeboy config set /lab/preferred_runner '\"<runner-id>\"'"
                        .to_string(),
                ]),
            )
        })?,
    };
    let runner_config = runner::load(&runner_id)?;
    let homeboy_path = runner_config
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let materialized_source = materialize_lab_extension_source(&runner_id, source)?;
    let install_command = runner_extension_install_command(
        &homeboy_path,
        &materialized_source.runner_source,
        extension_id,
        revision,
        replace,
    );
    let (execution, exit_code) = runner::exec(
        &runner_id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: true,
            command: install_command.clone(),
            env: runner_config.env.clone(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: materialized_source.source_snapshot.clone(),
            capability_preflight: Some(homeboy::core::runners::RunnerCapabilityPreflight {
                command: "homeboy lab extension-sync".to_string(),
                required_tools: vec![RunnerRequiredTool::Homeboy],
                required_commands: Vec::new(),
                required_components: Vec::new(),
                required_env: Vec::new(),
            }),
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        },
    )?;

    if exit_code == 0 {
        let installed_revision = installed_extension_source_revision(&execution.stdout)
            .ok_or_else(|| extension_sync_revision_error(extension_id, revision, None))?;
        if !revision_matches(revision, &installed_revision) {
            return Err(extension_sync_revision_error(
                extension_id,
                revision,
                Some(installed_revision),
            ));
        }
    }

    if let Some(err) = runner_exec_failure_error(&execution) {
        return Err(err);
    }

    Ok((
        LabCommandOutput::ExtensionSync(LabExtensionSyncOutput {
            command: "lab.extension_sync",
            runner_id,
            runner_homeboy_path: homeboy_path,
            extension_id: extension_id.to_string(),
            source: source.to_string(),
            runner_source: (materialized_source.runner_source != source)
                .then_some(materialized_source.runner_source),
            source_revision: revision.to_string(),
            replace,
            install_command,
            execution,
        }),
        exit_code,
    ))
}

struct MaterializedLabExtensionSource {
    runner_source: String,
    source_snapshot: Option<SourceSnapshot>,
}

fn materialize_lab_extension_source(
    runner_id: &str,
    source: &str,
) -> homeboy::core::Result<MaterializedLabExtensionSource> {
    let Some(local_source) = controller_local_source_path(source) else {
        return Ok(MaterializedLabExtensionSource {
            runner_source: source.to_string(),
            source_snapshot: None,
        });
    };

    let (synced, _) = runner::sync_workspace(
        runner_id,
        RunnerWorkspaceSyncOptions {
            path: local_source.display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
        },
    )?;

    Ok(MaterializedLabExtensionSource {
        source_snapshot: Some(SourceSnapshot::collect_local(
            runner_id,
            Path::new(&synced.local_path),
            Some(&synced.remote_path),
            "lab_extension_sync",
        )),
        runner_source: synced.remote_path,
    })
}

fn controller_local_source_path(source: &str) -> Option<PathBuf> {
    if looks_like_remote_source(source) {
        return None;
    }

    let expanded = shellexpand::tilde(source).to_string();
    let path = Path::new(&expanded);
    path.is_dir().then(|| path.canonicalize().ok()).flatten()
}

fn looks_like_remote_source(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    lower.contains("://") || lower.starts_with("git@") || lower.starts_with("ssh://")
}

fn runner_extension_install_command(
    homeboy_path: &str,
    source: &str,
    extension_id: &str,
    revision: &str,
    replace: bool,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source.to_string(),
        "--id".to_string(),
        extension_id.to_string(),
        "--ref".to_string(),
        revision.to_string(),
    ];
    if replace {
        command.push("--replace".to_string());
    }
    command
}

fn installed_extension_source_revision(stdout: &str) -> Option<String> {
    let value = parse_trailing_json(stdout)?;
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        return None;
    }

    let extension = value
        .get("data")
        .and_then(|data| data.get("extension"))
        .or_else(|| value.get("data"));

    extension
        .and_then(|data| data.get("source_revision"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|revision| !revision.is_empty())
        .map(str::to_string)
}

fn parse_trailing_json(stdout: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(stdout) {
        return Some(value);
    }

    stdout
        .char_indices()
        .rev()
        .filter(|(_, ch)| *ch == '{')
        .find_map(|(index, _)| serde_json::from_str(&stdout[index..]).ok())
}

fn revision_matches(requested: &str, installed: &str) -> bool {
    requested == installed || requested.starts_with(installed) || installed.starts_with(requested)
}

fn extension_sync_revision_error(
    extension_id: &str,
    requested: &str,
    installed: Option<String>,
) -> Error {
    let installed_display = installed.as_deref().unwrap_or("<missing>");
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Runner extension '{}' did not install requested ref {}; installed source revision is {}",
            extension_id, requested, installed_display
        ),
        Some(requested.to_string()),
        Some(vec![
            "Inspect the runner command stdout/stderr before rerunning downstream Lab tasks."
                .to_string(),
            format!(
                "Verify with: homeboy runner exec <runner> -- homeboy extension show {}",
                extension_id
            ),
        ]),
    )
}

fn lab_followups(runner_id: Option<&str>, current_workspace: Option<&str>) -> Vec<LabFollowup> {
    let Some(runner_id) = runner_id else {
        return Vec::new();
    };
    let runner_arg = shell_arg(runner_id);
    let mut followups = vec![
        LabFollowup {
            label: "doctor",
            command: format!("homeboy runner doctor {runner_arg}"),
            purpose: "Probe runner tools, workspace writability, artifact storage, and browser readiness without raw SSH.",
        },
        LabFollowup {
            label: "env",
            command: format!("homeboy runner env {runner_arg}"),
            purpose: "Show the redacted environment Homeboy injects into runner jobs.",
        },
        LabFollowup {
            label: "exec",
            command: format!("homeboy runner exec {runner_arg} -- <command>"),
            purpose: "Run a managed follow-up command through Homeboy instead of opening an ad-hoc shell.",
        },
    ];

    if let Some(path) = current_workspace {
        followups.push(LabFollowup {
            label: "workspace_sync",
            command: format!(
                "homeboy runner workspace sync {runner_arg} --path {} --mode snapshot",
                shell_arg(path)
            ),
            purpose: "Materialize the current checkout into the Lab runner workspace before a replay or follow-up run.",
        });
    }

    followups
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{
        controller_local_source_path, installed_extension_source_revision, lab_followups,
        revision_matches, runner_extension_install_command,
    };
    use std::fs;

    #[test]
    fn lab_extension_sync_command_replaces_by_default() {
        assert_eq!(
            runner_extension_install_command(
                "/home/chubes/.cargo/bin/homeboy",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
                "wordpress",
                "5842f0e",
                true,
            ),
            vec![
                "/home/chubes/.cargo/bin/homeboy",
                "extension",
                "install",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
                "--id",
                "wordpress",
                "--ref",
                "5842f0e",
                "--replace",
            ]
        );
    }

    #[test]
    fn lab_extension_sync_command_can_install_without_replace() {
        assert_eq!(
            runner_extension_install_command(
                "homeboy",
                "/home/chubes/Developer/homeboy-extensions/wordpress",
                "wordpress",
                "main",
                false,
            ),
            vec![
                "homeboy",
                "extension",
                "install",
                "/home/chubes/Developer/homeboy-extensions/wordpress",
                "--id",
                "wordpress",
                "--ref",
                "main",
            ]
        );
    }

    #[test]
    fn lab_extension_sync_reads_installed_revision_from_replace_output() {
        let stdout = r#"{
  "success": true,
  "data": {
    "command": "extension.replace",
    "extension_id": "wordpress",
    "source_revision": "941bf8c"
  }
}"#;

        assert_eq!(
            installed_extension_source_revision(stdout).as_deref(),
            Some("941bf8c")
        );
    }

    #[test]
    fn lab_extension_sync_detects_controller_local_source_directories() {
        let tempdir = tempfile::tempdir().expect("creates temp extension source");
        fs::write(tempdir.path().join("homeboy-extension.json"), "{}").expect("writes marker");
        let expected = tempdir.path().canonicalize().expect("canonical tempdir");

        assert_eq!(
            controller_local_source_path(tempdir.path().to_str().unwrap()).as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    fn lab_extension_sync_reads_installed_revision_after_setup_logs() {
        let stdout = r#"Preparing extension runtime...
Installing declared dependencies...
{
  "success": true,
  "data": {
    "command": "extension.replace",
    "extension_id": "wordpress",
    "source_revision": "941bf8cf"
  }
}
"#;

        assert_eq!(
            installed_extension_source_revision(stdout).as_deref(),
            Some("941bf8cf")
        );
    }

    #[test]
    fn lab_extension_sync_leaves_urls_and_runner_local_paths_unmaterialized() {
        assert!(controller_local_source_path(
            "https://github.com/Extra-Chill/homeboy-extensions.git"
        )
        .is_none());
        assert!(
            controller_local_source_path("git@github.com:Extra-Chill/homeboy-extensions.git")
                .is_none()
        );
        assert!(
            controller_local_source_path("/runner/only/homeboy-extensions/wordpress").is_none()
        );
    }

    #[test]
    fn lab_extension_sync_reads_installed_revision_from_show_style_output() {
        let stdout = r#"{
  "success": true,
  "data": {
    "extension": {
      "id": "wordpress",
      "source_revision": "941bf8c"
    }
  }
}"#;

        assert_eq!(
            installed_extension_source_revision(stdout).as_deref(),
            Some("941bf8c")
        );
    }

    #[test]
    fn lab_extension_sync_accepts_short_full_revision_matches() {
        assert!(revision_matches(
            "941bf8cff9f88758123db837ed12bb6f6de5d00f",
            "941bf8c"
        ));
        assert!(revision_matches(
            "941bf8c",
            "941bf8cff9f88758123db837ed12bb6f6de5d00f"
        ));
        assert!(!revision_matches("941bf8c", "f36543e"));
    }

    #[test]
    fn lab_followups_name_managed_runner_commands() {
        let followups = lab_followups(Some("homeboy-lab"), Some("/tmp/example workspace"));
        let commands: Vec<_> = followups.iter().map(|step| step.command.as_str()).collect();

        assert!(commands.contains(&"homeboy runner doctor homeboy-lab"));
        assert!(commands.contains(&"homeboy runner env homeboy-lab"));
        assert!(commands.contains(&"homeboy runner exec homeboy-lab -- <command>"));
        assert!(commands.contains(
            &"homeboy runner workspace sync homeboy-lab --path '/tmp/example workspace' --mode snapshot"
        ));
    }

    #[test]
    fn lab_followups_are_empty_without_a_selected_runner() {
        assert!(lab_followups(None, Some("/tmp/workspace")).is_empty());
    }
}
