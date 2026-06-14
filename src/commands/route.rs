use homeboy::cli_surface::{Cli, Commands};
use homeboy::command_contract::{
    LabCommandPortability, LabCommandRequiredTool, LabWorkspaceModePolicy,
};
use homeboy::core::observation::RunStatus;
use homeboy::core::runners;
use serde_json::json;
use std::time::Duration;

const DEFAULT_LAB_TRACE_DISPATCH_TIMEOUT_SECS: u64 = 9 * 60;
const LAB_TRACE_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_TRACE_DISPATCH_TIMEOUT_SECS";

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if is_lab_offload_subprocess() {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Runs(args)) = (cli.runner.as_deref(), &cli.command) {
        if !is_runs_list_runner_option(normalized_args) {
            return Err(crate::commands::runs::global_runner_error(args, runner_id));
        }

        return Ok(None);
    }

    let lab_command = lab_offload_command(&cli.command)?;

    let trace_runner_id = if matches!(cli.command, Commands::Trace(_)) {
        cli.runner
            .clone()
            .or_else(|| runners::resolve_default_lab_runner().ok().flatten())
    } else {
        None
    };

    let trace_observation = match &cli.command {
        Commands::Trace(args) => crate::commands::trace::start_lab_dispatch_observation(
            args,
            normalized_args,
            trace_runner_id.as_deref(),
        ),
        _ => None,
    };

    let lab_result = if matches!(cli.command, Commands::Trace(_)) {
        execute_trace_lab_offload_with_timeout(
            lab_command,
            normalized_args.to_vec(),
            cli.runner.clone(),
            cli.force_hot,
            cli.allow_local_hot,
            cli.allow_local_fallback,
            cli.command.lab_offload_mutation_flag().is_some(),
            lab_trace_dispatch_timeout(),
        )
    } else {
        runners::execute_lab_offload(runners::LabOffloadRequest {
            command: lab_command,
            normalized_args,
            explicit_runner: cli.runner.as_deref(),
            force_hot: cli.force_hot,
            allow_local_hot: cli.allow_local_hot,
            allow_local_fallback: cli.allow_local_fallback,
            capture_patch: cli.command.lab_offload_mutation_flag().is_some(),
        })
    };

    match lab_result {
        Err(err) => {
            crate::commands::trace::finish_lab_dispatch_observation(
                trace_observation,
                RunStatus::Error,
                json!({
                    "lab_dispatch": {
                        "phase": "route_lab_dispatch",
                        "runner_id": trace_runner_id,
                        "status": "error",
                        "error": {
                            "code": err.code.as_str(),
                            "message": err.message,
                            "details": err.details,
                            "hints": err.hints,
                        }
                    }
                }),
            );
            return Err(err);
        }
        Ok(outcome) => match outcome {
            runners::LabOffloadOutcome::RunLocal {
                metadata, messages, ..
            } => {
                crate::commands::trace::finish_lab_dispatch_observation(
                    trace_observation,
                    RunStatus::Skipped,
                    json!({
                        "lab_dispatch": {
                            "phase": "route_lab_dispatch",
                            "runner_id": trace_runner_id,
                            "status": "run_local",
                            "metadata": metadata,
                            "messages": messages,
                        }
                    }),
                );
                if let Some(metadata) = metadata {
                    runners::capture_lab_offload_subprocess_metadata(metadata);
                }
                for message in messages {
                    eprintln!("{message}");
                }
                Ok(None)
            }
            runners::LabOffloadOutcome::Offloaded {
                stdout,
                stderr,
                exit_code,
                ..
            } => {
                crate::commands::trace::finish_lab_dispatch_observation(
                    trace_observation,
                    if exit_code == 0 {
                        RunStatus::Pass
                    } else {
                        RunStatus::Fail
                    },
                    json!({
                        "lab_dispatch": {
                            "phase": "route_lab_dispatch",
                            "runner_id": trace_runner_id,
                            "status": "offloaded_complete",
                            "exit_code": exit_code,
                        }
                    }),
                );
                if !stderr.is_empty() {
                    eprint!("{stderr}");
                }
                if let Some(path) = output_file {
                    write_offloaded_stdout(path, &stdout)?;
                }
                print!("{stdout}");
                Ok(Some(exit_code))
            }
        },
    }
}

fn is_runs_list_runner_option(args: &[String]) -> bool {
    let Some(runs_index) = args.iter().position(|arg| arg == "runs") else {
        return false;
    };
    let Some(list_index) = args.iter().position(|arg| arg == "list") else {
        return false;
    };

    list_index > runs_index
        && args.iter().enumerate().any(|(index, arg)| {
            index > list_index && (arg == "--runner" || arg.starts_with("--runner="))
        })
}

fn execute_trace_lab_offload_with_timeout(
    command: Option<runners::LabOffloadCommand>,
    normalized_args: Vec<String>,
    explicit_runner: Option<String>,
    force_hot: bool,
    allow_local_hot: bool,
    allow_local_fallback: bool,
    capture_patch: bool,
    timeout: Duration,
) -> homeboy::core::Result<runners::LabOffloadOutcome> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = runners::execute_lab_offload(runners::LabOffloadRequest {
            command,
            normalized_args: &normalized_args,
            explicit_runner: explicit_runner.as_deref(),
            force_hot,
            allow_local_hot,
            allow_local_fallback,
            capture_patch,
        });
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout).map_err(|_| {
        homeboy::core::Error::internal_unexpected(format!(
            "Lab trace dispatch did not finish before timeout after {}s",
            timeout.as_secs()
        ))
        .with_hint("Inspect the controller trace run record for the last Lab dispatch phase.".to_string())
        .with_hint("Run `homeboy runner doctor <runner-id>` and retry after the runner is healthy.".to_string())
        .with_hint("Use `--force-hot --allow-local-hot` only for development-only investigation while fixing Lab routing.".to_string())
    })?
}

fn lab_trace_dispatch_timeout() -> Duration {
    std::env::var(LAB_TRACE_DISPATCH_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_LAB_TRACE_DISPATCH_TIMEOUT_SECS))
}

fn is_lab_offload_subprocess() -> bool {
    std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV)
        .is_ok_and(|value| !value.trim().is_empty())
}

fn write_offloaded_stdout(path: &str, stdout: &str) -> homeboy::core::Result<()> {
    std::fs::write(path, stdout).map_err(|err| {
        homeboy::core::Error::internal_io(err.to_string(), Some(format!("write {path}")))
    })
}

fn lab_offload_command(
    command: &Commands,
) -> homeboy::core::Result<Option<runners::LabOffloadCommand>> {
    let Some(contract) = command.lab_contract() else {
        return Ok(None);
    };
    let required_extensions = if contract.requires_extension_parity {
        lab_required_extensions(command)?
    } else {
        Vec::new()
    };
    Ok(Some(runners::LabOffloadCommand {
        hot_label: contract.hot_label,
        portable: matches!(contract.portability, LabCommandPortability::Portable),
        default_lab_offload: contract.default_lab_offload,
        unsupported_reason: match contract.portability {
            LabCommandPortability::Portable => None,
            LabCommandPortability::LocalOnly(reason) => Some(reason),
        },
        workspace_mode_policy: match contract.workspace_mode_policy {
            LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
                runners::LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            }
            LabWorkspaceModePolicy::Git => runners::LabOffloadWorkspaceModePolicy::Git,
            LabWorkspaceModePolicy::GitCheckoutRequired => {
                runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
            }
        },
        requires_extension_parity: contract.requires_extension_parity,
        required_extensions,
        requires_playwright: contract
            .extra_required_tools
            .iter()
            .any(|tool| matches!(tool, LabCommandRequiredTool::Playwright)),
        infer_source_path_tools: contract.infer_source_path_tools,
    }))
}

fn lab_required_extensions(command: &Commands) -> homeboy::core::Result<Vec<String>> {
    let mut extension_ids = std::collections::BTreeSet::new();

    match command {
        Commands::Audit(args) => extension_ids.extend(args.extension_override.extensions.clone()),
        Commands::Bench(args) => {
            extension_ids.extend(args.extension_override_ids().iter().cloned())
        }
        Commands::Lint(args) => extension_ids.extend(args.extension_override.extensions.clone()),
        Commands::Test(args) => {
            extension_ids.extend(args.extension_override.extensions.clone());
            extension_ids.extend(test_lab_extension_ids(args)?);
        }
        Commands::AgentTask(args) => extension_ids.extend(agent_task_lab_extension_ids(args)?),
        _ => {}
    }

    Ok(extension_ids.into_iter().collect())
}

fn agent_task_lab_extension_ids(
    args: &homeboy::commands::agent_task::AgentTaskArgs,
) -> homeboy::core::Result<Vec<String>> {
    let homeboy::commands::agent_task::AgentTaskCommand::RunPlan(run_plan) = &args.command else {
        return Ok(Vec::new());
    };
    if run_plan.plan.trim() == "-" {
        return Ok(Vec::new());
    }
    let raw = homeboy::core::config::read_json_spec_to_string(&run_plan.plan)?;
    let plan: homeboy::core::agent_tasks::AgentTaskPlan =
        serde_json::from_str(&raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task run-plan Lab extension inference".to_string()),
                Some(raw.clone()),
            )
        })?;

    Ok(homeboy::core::agent_tasks::required_extension_ids_for_plan(
        &plan,
    ))
}

fn test_lab_extension_ids(
    args: &homeboy::commands::test::TestArgs,
) -> homeboy::core::Result<Vec<String>> {
    let source_context = homeboy::core::engine::execution_context::resolve(
        &homeboy::core::engine::execution_context::ResolveOptions {
            component_id: args.comp.component.clone(),
            path_override: args.comp.path.clone(),
            capability: None,
            settings_overrides: args.setting_args.setting.clone(),
            settings_json_overrides: args.setting_args.setting_json.clone(),
            extension_overrides: args.extension_override.extensions.clone(),
        },
    )?;

    if !args.drift
        && args.ci_job.is_none()
        && source_context
            .component
            .has_script(homeboy::core::extension::ExtensionCapability::Test)
    {
        return Ok(Vec::new());
    }

    let context = homeboy::core::engine::execution_context::resolve(
        &homeboy::core::engine::execution_context::ResolveOptions {
            component_id: args.comp.component.clone(),
            path_override: args.comp.path.clone(),
            capability: Some(homeboy::core::extension::ExtensionCapability::Test),
            settings_overrides: args.setting_args.setting.clone(),
            settings_json_overrides: args.setting_args.setting_json.clone(),
            extension_overrides: args.extension_override.extensions.clone(),
        },
    )?;

    Ok(context.extension_id.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    struct EnvGuard {
        name: &'static str,
        previous: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self {
                name,
                previous,
                _guard: guard,
            }
        }

        fn remove(name: &'static str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(name).ok();
            std::env::remove_var(name);
            Self {
                name,
                previous,
                _guard: guard,
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn non_lab_command_continues_local_dispatch() {
        let cli = Cli::parse_from(["homeboy", "status"]);

        let outcome = route_after_parse(&cli, &["homeboy".into(), "status".into()], None).unwrap();

        assert_eq!(outcome, None);
    }

    #[test]
    fn hot_local_only_command_records_lab_plan_metadata() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let cli = Cli::parse_from(["homeboy", "lint", "--changed-since", "origin/main"]);

        let outcome = route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "lint".into(),
                "--changed-since".into(),
                "origin/main".into(),
            ],
            None,
        )
        .unwrap();

        assert_eq!(outcome, None);
        let raw = std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV)
            .expect("Lab routing metadata captured");
        let metadata: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(metadata["status"], "skipped");
        assert_eq!(metadata["source"], "automatic");
        assert!(metadata["plan_id"]
            .as_str()
            .unwrap()
            .contains("lab_offload"));
        assert!(metadata["fallback_reason"]
            .as_str()
            .unwrap()
            .contains("Changed-scope lint runs stay local"));
    }

    #[test]
    fn explicit_runner_for_local_only_hot_command_errors_from_lab_plan_path() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "test",
            "--changed-since",
            "origin/main",
        ]);

        let err = route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "test".into(),
                "--changed-since".into(),
                "origin/main".into(),
            ],
            None,
        )
        .expect_err("local-only hot command rejects explicit runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("--runner is unavailable"));
        assert!(err.message.contains("test --changed-since"));
    }

    #[test]
    fn lab_offload_subprocess_skips_recursive_lab_routing() {
        let _env = EnvGuard::set(
            homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV,
            r#"{"status":"offloaded"}"#,
        );
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "trace",
            "--rig",
            "gutenberg-pattern-preview-assets",
            "gutenberg",
            "pattern-preview-assets",
        ]);
        let normalized = [
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "trace".to_string(),
            "--rig".to_string(),
            "gutenberg-pattern-preview-assets".to_string(),
            "gutenberg".to_string(),
            "pattern-preview-assets".to_string(),
        ];

        let outcome = route_after_parse(&cli, &normalized, None).unwrap();

        assert_eq!(outcome, None);
    }

    #[test]
    fn trace_lab_dispatch_timeout_reads_env_override() {
        let _env = EnvGuard::set(LAB_TRACE_DISPATCH_TIMEOUT_ENV, "7");

        assert_eq!(lab_trace_dispatch_timeout(), Duration::from_secs(7));
    }

    #[test]
    fn offloaded_stdout_write_preserves_bytes_for_output_file() {
        let dir = tempdir().unwrap();
        let output_path = dir.path().join("out.json");

        write_offloaded_stdout(&output_path.to_string_lossy(), "{\"ok\":true}\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(output_path).unwrap(),
            "{\"ok\":true}\n"
        );
    }

    #[test]
    fn lab_command_preserves_portable_contract_shape() {
        let cli = Cli::parse_from(["homeboy", "lint"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "lint");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
        assert!(command.requires_extension_parity);
    }

    #[test]
    fn extension_update_requires_explicit_lab_runner_without_extension_parity() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "lab",
            "extension",
            "update",
            "wordpress",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "extension update");
        assert!(command.portable);
        assert!(!command.default_lab_offload);
        assert!(command.unsupported_reason.is_none());
        assert!(!command.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.infer_source_path_tools);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn extension_update_routes_locally_without_explicit_lab_runner() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "extension".to_string(),
            "update".to_string(),
            "wordpress".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("extension update without --runner should not offload");

        assert_eq!(outcome, None);
        assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
    }

    #[test]
    fn other_extension_commands_stay_local_only() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "lab",
            "extension",
            "show",
            "wordpress",
        ]);

        assert!(lab_offload_command(&cli.command).unwrap().is_none());
        assert!(!cli.command.supports_lab_runner());
    }

    #[test]
    fn global_runner_for_runs_show_has_local_mirror_guidance() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "runs",
            "show",
            "run-123",
        ]);

        let err = route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "show".into(),
                "run-123".into(),
            ],
            None,
        )
        .expect_err("runs show rejects global runner with guidance");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("homeboy runs show run-123"));
        assert!(err.message.contains("without --runner"));
    }

    #[test]
    fn runs_list_runner_option_after_subcommand_routes_locally() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);

        for normalized in [
            vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "list".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "--status".to_string(),
                "running".to_string(),
                "--limit".to_string(),
                "20".to_string(),
            ],
            vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "list".to_string(),
                "--runner=homeboy-lab".to_string(),
                "--status".to_string(),
                "running".to_string(),
                "--limit".to_string(),
                "20".to_string(),
            ],
        ] {
            let cli = Cli::parse_from(&normalized);

            let outcome = route_after_parse(&cli, &normalized, None)
                .expect("runs list subcommand runner option should not be rejected");

            assert_eq!(outcome, None);
        }
    }

    #[test]
    fn global_runner_for_runs_list_keeps_placement_guidance() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "runs".to_string(),
            "list".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let err = route_after_parse(&cli, &normalized, None)
            .expect_err("top-level runner on runs list should keep placement guidance");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("homeboy runs list --runner homeboy-lab"));
    }

    #[test]
    fn agent_task_inspection_commands_are_read_only_lab_portable() {
        for args in [
            ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            let command = lab_offload_command(&cli.command).unwrap().unwrap();

            assert_eq!(command.hot_label, "agent-task status/logs/artifacts");
            assert!(command.portable);
            assert!(!command.requires_extension_parity);
            assert!(command.required_extensions.is_empty());
            assert!(!command.infer_source_path_tools);
        }
    }

    #[test]
    fn lab_command_with_mutation_flag_stays_portable_for_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "audit", "--baseline"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "audit");
        assert!(command.portable);
        assert_eq!(command.unsupported_reason, None);
        assert!(command.requires_extension_parity);
    }

    #[test]
    fn lab_command_with_ratchet_stays_portable_for_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "audit", "--ratchet"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "audit");
        assert!(command.portable);
        assert_eq!(command.unsupported_reason, None);
        assert!(command.requires_extension_parity);
    }

    #[test]
    fn lab_command_preserves_local_only_contract_shape() {
        let cli = Cli::parse_from(["homeboy", "rig", "up", "demo"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "rig up");
        assert!(!command.portable);
        assert!(command.unsupported_reason.is_some());
        assert!(!command.requires_extension_parity);
    }
}
