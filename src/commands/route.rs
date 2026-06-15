use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::lab_routing::{self, LabRoutingRequest};
use homeboy::core::observation::RunStatus;
use homeboy::core::runners::{self, RunnerExecOptions};
use serde_json::json;
use std::collections::HashMap;

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if lab_routing::is_lab_offload_subprocess() {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Runs(args)) = (cli.runner.as_deref(), &cli.command) {
        if !is_runs_list_runner_option(normalized_args) {
            return Err(crate::commands::runs::global_runner_error(args, runner_id));
        }

        return Ok(None);
    }

    if is_lab_command_local_runner_option(&cli.command) {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Rig(args)) = (cli.runner.as_deref(), &cli.command) {
        if args.is_runner_source_management_command() {
            let (stdout, stderr, exit_code) =
                run_rig_source_management_on_runner(runner_id, normalized_args, output_file)?;
            if !stderr.is_empty() {
                eprint!("{stderr}");
            }
            print!("{stdout}");
            return Ok(Some(exit_code));
        }
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

    let mutation_flag = cli.command.lab_offload_mutation_flag();
    let lab_result = lab_routing::route_lab_offload(LabRoutingRequest {
        command: lab_command,
        normalized_args,
        explicit_runner: cli.runner.as_deref(),
        force_hot: cli.force_hot,
        allow_local_hot: cli.allow_local_hot,
        allow_local_fallback: cli.allow_local_fallback,
        allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
        capture_patch: mutation_flag.is_some(),
        mutation_flag,
        timeout: None,
        active_run_id: crate::commands::trace::lab_dispatch_observation_run_id(&trace_observation),
    });

    match lab_result {
        Err(err) => {
            let _ = crate::commands::trace::finish_lab_dispatch_observation(
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
                let _ = crate::commands::trace::finish_lab_dispatch_observation(
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
                let retrieval = crate::commands::trace::finish_lab_dispatch_observation(
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
                let stdout = stdout_with_persisted_run_retrieval(&stdout, retrieval.as_ref());
                if let Some(path) = output_file {
                    write_offloaded_stdout(path, &stdout)?;
                }
                print!("{stdout}");
                Ok(Some(exit_code))
            }
        },
    }
}

fn run_rig_source_management_on_runner(
    runner_id: &str,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<(String, String, i32)> {
    let runner = runners::load(runner_id)?;
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let command = runner_rig_source_management_command(homeboy_path, normalized_args);
    let (output, exit_code) = runners::exec(
        runner_id,
        RunnerExecOptions {
            cwd: runner.workspace_root.clone(),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env: HashMap::new(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        },
    )?;

    if let Some(path) = output_file {
        write_offloaded_stdout(path, &output.stdout)?;
    }

    Ok((output.stdout, output.stderr, exit_code))
}

fn runner_rig_source_management_command(
    homeboy_path: &str,
    normalized_args: &[String],
) -> Vec<String> {
    let mut command = vec![homeboy_path.to_string()];
    let mut iter = normalized_args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--runner" || arg == "--output" || arg == "--artifact-root" {
            iter.next();
            continue;
        }
        if arg == "--allow-local-fallback"
            || arg == "--allow-dirty-lab-workspace"
            || arg == "--allow-local-hot"
        {
            continue;
        }
        if arg.starts_with("--runner=")
            || arg.starts_with("--output=")
            || arg.starts_with("--artifact-root=")
        {
            continue;
        }
        command.push(arg.clone());
    }
    command
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

fn is_lab_command_local_runner_option(command: &Commands) -> bool {
    matches!(command, Commands::Lab(_))
}

fn write_offloaded_stdout(path: &str, stdout: &str) -> homeboy::core::Result<()> {
    std::fs::write(path, stdout).map_err(|err| {
        homeboy::core::Error::internal_io(err.to_string(), Some(format!("write {path}")))
    })
}

fn stdout_with_persisted_run_retrieval(
    stdout: &str,
    retrieval: Option<&crate::commands::trace::PersistedRunRetrieval>,
) -> String {
    let Some(retrieval) = retrieval else {
        return stdout.to_string();
    };

    if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(stdout) {
        attach_persisted_run_retrieval_json(&mut json, retrieval);
        if let Ok(mut rendered) = serde_json::to_string_pretty(&json) {
            rendered.push('\n');
            return rendered;
        }
    }

    let mut rendered = stdout.to_string();
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push('\n');
    rendered.push_str("# Homeboy persisted run\n\n");
    rendered.push_str(&format!(
        "- **Persisted Homeboy run ID:** `{}`\n",
        retrieval.run_id
    ));
    rendered.push_str("- **ID scope:** runtime, temp, and artifact identifiers above are offload context; use the persisted Homeboy run ID for local retrieval.\n");
    rendered.push_str("- **Retrieve evidence:** `");
    rendered.push_str(&retrieval.evidence_command);
    rendered.push_str("`\n");
    rendered.push_str("- **List artifacts:** `");
    rendered.push_str(&retrieval.artifacts_command);
    rendered.push_str("`\n");
    rendered.push_str("- **Export run bundle:** `");
    rendered.push_str(&retrieval.export_command);
    rendered.push_str("`\n");
    rendered
}

fn attach_persisted_run_retrieval_json(
    json: &mut serde_json::Value,
    retrieval: &crate::commands::trace::PersistedRunRetrieval,
) {
    let retrieval_json = retrieval.to_json();
    if let Some(object) = json.as_object_mut() {
        object.insert("homeboy_persisted_run".to_string(), retrieval_json);
    }
}

fn lab_offload_command(
    command: &Commands,
) -> homeboy::core::Result<Option<runners::LabOffloadCommand>> {
    let Some(contract) = command.lab_contract() else {
        return Ok(None);
    };
    let required_extensions = command.lab_required_extensions()?;
    Ok(Some(lab_routing::lab_offload_command_from_contract(
        contract,
        required_extensions,
    )))
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
    fn lab_extension_sync_runner_option_routes_to_lab_command_handler() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "lab".to_string(),
            "extension-sync".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--source".to_string(),
            "/tmp/wordpress-extension".to_string(),
            "--id".to_string(),
            "wordpress".to_string(),
            "--ref".to_string(),
            "main".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("lab extension-sync owns its runner option locally");

        assert_eq!(outcome, None);
    }

    #[test]
    fn trace_lab_dispatch_timeout_reads_env_override() {
        let _env = EnvGuard::set(lab_routing::LAB_TRACE_DISPATCH_TIMEOUT_ENV, "7");

        assert_eq!(
            lab_routing::lab_trace_dispatch_timeout(),
            std::time::Duration::from_secs(7)
        );
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
    fn offloaded_json_stdout_labels_persisted_homeboy_run_id() {
        let retrieval = crate::commands::trace::PersistedRunRetrieval::for_run("trace-run-123");

        let stdout = stdout_with_persisted_run_retrieval(
            r#"{"success":true,"data":{"runtime_id":"runtime-abc","artifact_id":"artifact-xyz"}}"#,
            Some(&retrieval),
        );
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");

        assert_eq!(json["data"]["runtime_id"], "runtime-abc");
        assert_eq!(json["data"]["artifact_id"], "artifact-xyz");
        assert_eq!(
            json["homeboy_persisted_run"]["persisted_run_id"],
            "trace-run-123"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["retrieval_commands"]["evidence"],
            "homeboy runs evidence trace-run-123"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["retrieval_commands"]["artifacts"],
            "homeboy runs artifacts trace-run-123"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["retrieval_commands"]["export"],
            "homeboy runs export --run trace-run-123 --output homeboy-run-trace-run-123"
        );
    }

    #[test]
    fn offloaded_error_json_stdout_labels_persisted_homeboy_run_id() {
        let retrieval = crate::commands::trace::PersistedRunRetrieval::for_run("trace-run-err");

        let stdout = stdout_with_persisted_run_retrieval(
            r#"{"success":false,"error":{"code":"remote.command_failed","message":"failed","details":{"temp_id":"tmp-1"}}}"#,
            Some(&retrieval),
        );
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");

        assert_eq!(json["error"]["details"]["temp_id"], "tmp-1");
        assert_eq!(
            json["homeboy_persisted_run"]["persisted_run_id"],
            "trace-run-err"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["id_scope"],
            "persisted_homeboy_run"
        );
    }

    #[test]
    fn offloaded_text_stdout_appends_persisted_run_retrieval_commands() {
        let retrieval = crate::commands::trace::PersistedRunRetrieval::for_run("trace-run-text");

        let stdout = stdout_with_persisted_run_retrieval(
            "runtime_id=runtime-abc\nartifact_id=artifact-xyz\n",
            Some(&retrieval),
        );

        assert!(stdout.contains("runtime_id=runtime-abc"));
        assert!(stdout.contains("artifact_id=artifact-xyz"));
        assert!(stdout.contains("**Persisted Homeboy run ID:** `trace-run-text`"));
        assert!(stdout.contains("`homeboy runs evidence trace-run-text`"));
        assert!(stdout.contains("`homeboy runs artifacts trace-run-text`"));
        assert!(stdout.contains(
            "`homeboy runs export --run trace-run-text --output homeboy-run-trace-run-text`"
        ));
    }

    #[test]
    fn runner_rig_source_management_command_strips_controller_globals() {
        let normalized = vec![
            "homeboy".to_string(),
            "rig".to_string(),
            "sources".to_string(),
            "list".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--output=./sources.json".to_string(),
            "--allow-local-fallback".to_string(),
        ];

        assert_eq!(
            runner_rig_source_management_command("/usr/local/bin/homeboy", &normalized),
            vec![
                "/usr/local/bin/homeboy".to_string(),
                "rig".to_string(),
                "sources".to_string(),
                "list".to_string(),
            ]
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
            ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            let command = lab_offload_command(&cli.command).unwrap().unwrap();

            assert_eq!(command.hot_label, "agent-task status/logs/artifacts/review");
            assert!(command.portable);
            assert!(!command.requires_extension_parity);
            assert!(command.required_extensions.is_empty());
            assert!(!command.infer_source_path_tools);
        }
    }

    #[test]
    fn agent_task_providers_supports_explicit_runner_discovery() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "providers",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(command.hot_label, "agent-task providers");
        assert!(command.portable);
        assert!(!command.default_lab_offload);
        assert!(!command.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.infer_source_path_tools);
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
