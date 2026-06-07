use homeboy::cli_surface::{Cli, Commands};

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    let lab_command = lab_offload_command(&cli.command)?;

    match homeboy::core::runner::execute_lab_offload(homeboy::core::runner::LabOffloadRequest {
        command: lab_command,
        normalized_args,
        explicit_runner: cli.runner.as_deref(),
        force_hot: cli.force_hot,
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
    use tempfile::tempdir;

    #[test]
    fn non_lab_command_continues_local_dispatch() {
        let cli = Cli::parse_from(["homeboy", "status"]);

        let outcome = route_after_parse(&cli, &["homeboy".into(), "status".into()], None).unwrap();

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
