use homeboy::cli_surface::{Cli, Commands};

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if is_lab_offload_subprocess() {
        return Ok(None);
    }

    let lab_command = lab_offload_command(&cli.command)?;

    match homeboy::core::runner::execute_lab_offload(homeboy::core::runner::LabOffloadRequest {
        command: lab_command,
        normalized_args,
        explicit_runner: cli.runner.as_deref(),
        force_hot: cli.force_hot,
        allow_local_hot: cli.allow_local_hot,
        allow_local_fallback: cli.allow_local_fallback,
        capture_patch: cli.command.lab_offload_mutation_flag().is_some(),
    })? {
        homeboy::core::runner::LabOffloadOutcome::RunLocal {
            metadata, messages, ..
        } => {
            if let Some(metadata) = metadata {
                homeboy::core::runner::capture_lab_offload_subprocess_metadata(metadata);
            }
            for message in messages {
                eprintln!("{message}");
            }
            Ok(None)
        }
        homeboy::core::runner::LabOffloadOutcome::Offloaded {
            stdout,
            stderr,
            exit_code,
            ..
        } => {
            if !stderr.is_empty() {
                eprint!("{stderr}");
            }
            if let Some(path) = output_file {
                write_offloaded_stdout(path, &stdout)?;
            }
            print!("{stdout}");
            Ok(Some(exit_code))
        }
    }
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
) -> homeboy::core::Result<Option<homeboy::core::runner::LabOffloadCommand>> {
    let Some(contract) = command.lab_contract() else {
        return Ok(None);
    };
    let required_extensions = if contract.requires_extension_parity {
        lab_required_extensions(command)?
    } else {
        Vec::new()
    };
    Ok(Some(homeboy::core::runner::LabOffloadCommand {
        hot_label: contract.hot_label,
        portable: matches!(
            contract.portability,
            homeboy::cli_surface::LabCommandPortability::Portable
        ),
        unsupported_reason: match contract.portability {
            homeboy::cli_surface::LabCommandPortability::Portable => None,
            homeboy::cli_surface::LabCommandPortability::LocalOnly(reason) => Some(reason),
        },
        workspace_mode_policy: match contract.workspace_mode_policy {
            homeboy::cli_surface::LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
                homeboy::core::runner::LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            }
        },
        requires_extension_parity: contract.requires_extension_parity,
        required_extensions,
        requires_playwright: contract.extra_required_tools.iter().any(|tool| {
            matches!(
                tool,
                homeboy::cli_surface::LabCommandRequiredTool::Playwright
            )
        }),
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
        _ => {}
    }

    Ok(extension_ids.into_iter().collect())
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
