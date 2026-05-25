use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};
use std::path::{Path, PathBuf};

use homeboy::cli_surface::Cli;
use homeboy::cli_surface::Commands;
use homeboy::commands::GlobalArgs;

use homeboy::commands;
use homeboy::commands::cli;
use homeboy::commands::utils::{args, entity_suggest, resource_policy, response as output};
use homeboy::core::extension::load_all_extensions;

struct ExtensionCliCommand {
    tool: String,
    project_id: String,
    args: Vec<String>,
}

struct ExtensionCliInfo {
    tool: String,
    display_name: String,
    extension_name: String,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
}

fn collect_extension_cli_info() -> Vec<ExtensionCliInfo> {
    load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            m.cli.map(|cli| {
                let help = cli.help.unwrap_or_default();
                ExtensionCliInfo {
                    tool: cli.tool,
                    display_name: cli.display_name,
                    extension_name: m.name,
                    project_id_help: help.project_id_help,
                    args_help: help.args_help,
                    examples: help.examples,
                }
            })
        })
        .collect()
}

fn build_augmented_command(extension_info: &[ExtensionCliInfo]) -> Command {
    let mut cmd = Cli::command();

    for info in extension_info {
        let project_id_help = info
            .project_id_help
            .clone()
            .unwrap_or_else(|| "Project ID".to_string());
        let args_help = info
            .args_help
            .clone()
            .unwrap_or_else(|| "Command arguments".to_string());

        let mut subcommand = Command::new(info.tool.clone())
            .about(format!(
                "Run {} commands via {}",
                info.display_name, info.extension_name
            ))
            .arg(
                clap::Arg::new("project_id")
                    .help(project_id_help)
                    .required(true)
                    .index(1),
            )
            .arg(
                clap::Arg::new("args")
                    .help(args_help)
                    .index(2)
                    .num_args(0..)
                    .allow_hyphen_values(true),
            )
            .trailing_var_arg(true);

        if !info.examples.is_empty() {
            let examples_text = format!("Examples:\n  {}", info.examples.join("\n  "));
            subcommand = subcommand.after_help(examples_text);
        }

        cmd = cmd.subcommand(subcommand);
    }

    cmd
}

fn try_parse_extension_cli_command(
    matches: &ArgMatches,
    extension_info: &[ExtensionCliInfo],
) -> Option<ExtensionCliCommand> {
    let (tool, sub_matches) = matches.subcommand()?;

    if !extension_info.iter().any(|m| m.tool == tool) {
        return None;
    }

    let project_id = sub_matches.get_one::<String>("project_id")?.clone();
    let args: Vec<String> = sub_matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Some(ExtensionCliCommand {
        tool: tool.to_string(),
        project_id,
        args,
    })
}

fn main() -> std::process::ExitCode {
    let extension_info = collect_extension_cli_info();
    let cmd = build_augmented_command(&extension_info);

    let args: Vec<String> = std::env::args().collect();
    let normalized = args::normalize(args);

    let matches = match cmd.try_get_matches_from(normalized.clone()) {
        Ok(m) => m,
        Err(e) => {
            if let Some(output) = try_augment_clap_error(&e) {
                eprintln!("{}", output);
                return std::process::ExitCode::from(2);
            }
            e.exit();
        }
    };

    let global = GlobalArgs {};

    // Extract --output early so it's available for all code paths (including
    // extension CLI commands which exit before Cli::from_arg_matches).
    let mut output_file: Option<String> = matches
        .try_get_one::<std::path::PathBuf>("output")
        .ok()
        .flatten()
        .map(|path| path.to_string_lossy().to_string());

    let artifact_root_override = matches
        .try_get_one::<std::path::PathBuf>("artifact_root")
        .ok()
        .flatten()
        .cloned();
    homeboy::core::set_artifact_root_override(artifact_root_override.clone());

    if let Some(extension_cmd) = try_parse_extension_cli_command(&matches, &extension_info) {
        let cli_args = cli::CliArgs {
            tool: extension_cmd.tool,
            identifier: extension_cmd.project_id,
            args: extension_cmd.args,
        };
        let result = cli::run(cli_args, &global);

        let (json_result, exit_code) = output::map_cmd_result_to_json(result);
        emit_json_result(json_result, output_file.as_deref(), exit_code);
        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
    }

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    if let Some(runner_id) = cli.runner.as_deref() {
        if !cli.command.supports_lab_runner() {
            let err = homeboy::core::Error::validation_invalid_argument(
                "runner",
                "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, and trace",
                Some(runner_id.to_string()),
                None,
            );
            emit_json_result(Err(err), output_file.as_deref(), 2);
            return std::process::ExitCode::from(exit_code_to_u8(2));
        }
        let capture_patch = cli.command.lab_offload_mutation_flag().is_some();
        return run_lab_offload(
            runner_id,
            &cli.command,
            &normalized,
            output_file.as_deref(),
            capture_patch,
        );
    }

    homeboy::core::set_artifact_root_override(cli.artifact_root.clone().or(artifact_root_override));

    if matches!(&cli.command, Commands::Runs(args) if args.is_bundle_export()) {
        output_file = None;
    }

    if let Some(hot_command) = resource_policy::hot_command(&cli.command) {
        if let Ok((resources, _)) = homeboy::commands::doctor::resources::run(
            homeboy::commands::doctor::resources::ResourcesArgs {},
        ) {
            let warning = resource_policy::evaluate(hot_command, &resources);
            if let Some(warning) = warning.as_ref() {
                if !cli.force_hot {
                    eprintln!("{}", warning.message);
                }
            }
            // Persist the preflight resource policy decision so observation
            // runs (bench, lint, test, etc.) can record it in their metadata
            // for later interpretation. This stays generic to Homeboy core.
            resource_policy::capture_context(
                resource_policy::ResourcePolicyContext::from_evaluation(
                    hot_command,
                    &resources,
                    warning.as_ref(),
                    cli.force_hot,
                ),
            );
        }
    }

    // Startup update checks — skip for upgrade (it handles this itself)
    if !matches!(
        &cli.command,
        Commands::Upgrade(_) | Commands::Daemon(_) | Commands::SelfCmd(_)
    ) {
        homeboy::core::upgrade::update_check::run_startup_check();
        homeboy::core::extension::update_check::run_startup_check();
    }

    if matches!(cli.command, Commands::List) {
        let mut cmd = build_augmented_command(&extension_info);
        cmd.print_help().expect("Failed to print help");
        println!();
        return std::process::ExitCode::SUCCESS;
    }

    // Show help for changelog when neither subcommand nor --self is provided
    if let Commands::Changelog(ref args) = cli.command {
        if args.command.is_none() && !args.show_self {
            let cmd = build_augmented_command(&extension_info);
            if let Some(mut changelog_cmd) = cmd.find_subcommand("changelog").cloned() {
                changelog_cmd.print_help().expect("Failed to print help");
                println!();
                return std::process::ExitCode::SUCCESS;
            }
        }
    }

    let exit_code = commands::response::run(cli.command, &global, output_file.as_deref());

    std::process::ExitCode::from(exit_code_to_u8(exit_code))
}

fn exit_code_to_u8(code: i32) -> u8 {
    if code <= 0 {
        0
    } else if code >= 255 {
        255
    } else {
        code as u8
    }
}

fn emit_json_result(
    result: homeboy::core::Result<serde_json::Value>,
    output_file: Option<&str>,
    exit_code: i32,
) {
    if let Some(path) = output_file {
        output::write_json_to_file(&result, path, exit_code);
    }
    output::print_json_result(result, exit_code).ok();
}

fn run_lab_offload(
    runner_id: &str,
    command: &Commands,
    normalized_args: &[String],
    output_file: Option<&str>,
    capture_patch: bool,
) -> std::process::ExitCode {
    match run_lab_offload_inner(
        runner_id,
        command,
        normalized_args,
        output_file,
        capture_patch,
    ) {
        Ok(exit_code) => std::process::ExitCode::from(exit_code_to_u8(exit_code)),
        Err(err) => {
            let (json_result, exit_code) =
                output::map_cmd_result_to_json::<serde_json::Value>(Err(err));
            emit_json_result(json_result, output_file, exit_code);
            std::process::ExitCode::from(exit_code_to_u8(exit_code))
        }
    }
}

fn run_lab_offload_inner(
    runner_id: &str,
    command_kind: &Commands,
    normalized_args: &[String],
    output_file: Option<&str>,
    capture_patch: bool,
) -> homeboy::core::Result<i32> {
    let runner = homeboy::core::runner::load(runner_id)?;
    if runner.kind != homeboy::core::runner::RunnerKind::Ssh {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote SSH runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register an SSH runner and run `homeboy runner connect <runner-id>` first."
                    .to_string(),
            ]),
        ));
    }

    let status = homeboy::core::runner::status(runner_id)?;
    if !status.connected {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a connected runner daemon",
            Some(runner_id.to_string()),
            Some(vec![format!(
                "Run `homeboy runner connect {runner_id}` before using --runner."
            )]),
        ));
    }

    runner.workspace_root.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "workspace_root",
            "Lab offload requires runner.workspace_root so the local checkout can be mapped remotely",
            Some(runner.id.clone()),
            Some(vec![
                "This Wave 3 adapter assumes workspace sync/provenance has placed the same checkout basename under runner.workspace_root.".to_string(),
            ]),
        )
    })?;
    let source_path = lab_offload_source_path(normalized_args)?;
    let synced = homeboy::core::runner::sync_workspace(
        runner_id,
        homeboy::core::runner::RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            mode: homeboy::core::runner::RunnerWorkspaceSyncMode::Snapshot,
        },
    )?
    .0;
    let remote_cwd = synced.remote_path;
    let source_snapshot = homeboy::core::source_snapshot::SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&remote_cwd),
        "lab_offload",
    );
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    preflight_lab_offload_test_extensions(command_kind, runner_id, homeboy_path, &remote_cwd)?;

    let mut command = vec![homeboy_path.to_string()];
    command.extend(
        rewrite_lab_offload_args(normalized_args, &remote_cwd)
            .into_iter()
            .skip(1),
    );

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        command.join(" "),
        runner_id,
        remote_cwd
    );
    let (exec_output, exit_code) = homeboy::core::runner::exec(
        runner_id,
        homeboy::core::runner::RunnerExecOptions {
            cwd: Some(remote_cwd),
            allow_ssh: false,
            command,
            capture_patch,
            source_snapshot: Some(source_snapshot),
        },
    )?;

    if !exec_output.stderr.is_empty() {
        eprint!("{}", exec_output.stderr);
    }
    if let Some(path) = output_file {
        std::fs::write(path, &exec_output.stdout).map_err(|err| {
            homeboy::core::Error::internal_io(err.to_string(), Some(format!("write {path}")))
        })?;
    }
    print!("{}", exec_output.stdout);
    Ok(exit_code)
}

fn preflight_lab_offload_test_extensions(
    command: &Commands,
    runner_id: &str,
    homeboy_path: &str,
    remote_cwd: &str,
) -> homeboy::core::Result<()> {
    let extension_ids = lab_offload_test_extension_ids(command)?;
    for extension_id in extension_ids {
        let (output, exit_code) = homeboy::core::runner::exec(
            runner_id,
            homeboy::core::runner::RunnerExecOptions {
                cwd: Some(remote_cwd.to_string()),
                allow_ssh: false,
                command: vec![
                    homeboy_path.to_string(),
                    "extension".to_string(),
                    "show".to_string(),
                    extension_id.clone(),
                ],
                capture_patch: false,
                source_snapshot: None,
            },
        )?;

        if exit_code != 0 {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "runner_extension",
                format!(
                    "Lab offload runner '{runner_id}' is missing required test extension '{extension_id}' before test execution"
                ),
                Some(extension_id.clone()),
                Some(vec![
                    format!(
                        "Install the extension on the runner before offloading tests: {homeboy_path} extension install <source> --id {extension_id}"
                    ),
                    format!(
                        "Remote preflight command failed: {homeboy_path} extension show {extension_id}"
                    ),
                    runner_extension_preflight_tail(&output.stderr, &output.stdout),
                ]),
            ));
        }
    }

    Ok(())
}

fn lab_offload_test_extension_ids(command: &Commands) -> homeboy::core::Result<Vec<String>> {
    let Commands::Test(args) = command else {
        return Ok(Vec::new());
    };

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

fn runner_extension_preflight_tail(stderr: &str, stdout: &str) -> String {
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let tail = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    if tail.is_empty() {
        "Runner extension preflight produced no diagnostic output.".to_string()
    } else {
        format!("Runner extension preflight output:\n{tail}")
    }
}

fn lab_offload_source_path(args: &[String]) -> homeboy::core::Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" {
            let value = iter.next().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "path",
                    "--path requires a value before Lab offload can sync the workspace",
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
    }

    std::env::current_dir().map_err(|err| {
        homeboy::core::Error::internal_io(err.to_string(), Some("read cwd".to_string()))
    })
}

fn rewrite_lab_offload_args(args: &[String], remote_path: &str) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--path" {
            stripped.push(arg.clone());
            let _ = iter.next();
            stripped.push(remote_path.to_string());
            continue;
        }
        if arg.starts_with("--path=") {
            stripped.push(format!("--path={remote_path}"));
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--output" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    stripped
}

/// Attempt to augment a clap error with entity suggestions.
/// Returns Some(augmented_message) if the unrecognized string matches a known entity.
fn try_augment_clap_error(e: &clap::Error) -> Option<String> {
    use clap::error::ErrorKind;

    // Only handle InvalidSubcommand errors
    if e.kind() != ErrorKind::InvalidSubcommand {
        return None;
    }

    // Extract unrecognized subcommand and parent command from error
    let unrecognized = extract_unrecognized_from_error(e)?;
    let parent_command = extract_parent_command_from_error(e)?;

    // Check if it matches a known entity
    let entity_match = entity_suggest::find_entity_match(&unrecognized)?;

    // Generate hints
    let hints =
        entity_suggest::generate_entity_hints(&entity_match, &parent_command, &unrecognized);

    // Build augmented output
    let mut output = format!("error: unrecognized subcommand '{}'\n\n", unrecognized);
    for hint in hints {
        output.push_str(&format!("hint: {}\n", hint));
    }
    output.push_str(&format!(
        "\nFor more information, try 'homeboy {} --help'",
        parent_command
    ));

    Some(output)
}

/// Extract the unrecognized subcommand string from a clap error.
fn extract_unrecognized_from_error(e: &clap::Error) -> Option<String> {
    use clap::error::ContextKind;

    // clap 4.x provides context via e.context()
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::InvalidSubcommand) {
            return Some(value.to_string());
        }
    }

    // Fallback: parse from error message
    // Format: "error: unrecognized subcommand 'xyz'"
    let msg = e.to_string();
    if let Some(start) = msg.find("unrecognized subcommand '") {
        let rest = &msg[start + 25..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }

    None
}

/// Extract the parent command from a clap error's usage string.
fn extract_parent_command_from_error(e: &clap::Error) -> Option<String> {
    use clap::error::ContextKind;

    // clap 4.x: look for Usage context which contains "homeboy <command> ..."
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::Usage) {
            let usage = value.to_string();
            // Format: "Usage: homeboy <command> [OPTIONS] ..."
            if let Some(rest) = usage.strip_prefix("Usage: homeboy ") {
                // Get first word after "homeboy "
                if let Some(cmd) = rest.split_whitespace().next() {
                    // Skip if it's a placeholder like "[OPTIONS]" or "<COMMAND>"
                    if !cmd.starts_with('[') && !cmd.starts_with('<') {
                        return Some(cmd.to_string());
                    }
                }
            }
        }
    }

    // Fallback: parse from error message which includes usage
    let msg = e.to_string();
    if let Some(start) = msg.find("Usage: homeboy ") {
        let rest = &msg[start + 15..];
        if let Some(cmd) = rest.split_whitespace().next() {
            if !cmd.starts_with('[') && !cmd.starts_with('<') {
                return Some(cmd.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        lab_offload_source_path, lab_offload_test_extension_ids, rewrite_lab_offload_args,
        runner_extension_preflight_tail,
    };
    use homeboy::cli_surface::Commands;
    use homeboy::commands::test::TestArgs;
    use homeboy::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs,
    };
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_args_for_path(path: &std::path::Path) -> TestArgs {
        TestArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: Some(path.to_string_lossy().to_string()),
            },
            extension_override: ExtensionOverrideArgs::default(),
            skip_lint: false,
            coverage: false,
            coverage_min: None,
            baseline_args: BaselineArgs::default(),
            analyze: false,
            drift: false,
            write: false,
            since: "HEAD~10".to_string(),
            changed_since: None,
            ci_job: None,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            _json: HiddenJsonArgs::default(),
            json_summary: false,
        }
    }

    fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let previous_home = std::env::var("HOME").ok();
        let home = tempfile::tempdir().expect("temp home");
        std::env::set_var("HOME", home.path());
        let result = f(home.path());
        if let Some(previous_home) = previous_home {
            std::env::set_var("HOME", previous_home);
        } else {
            std::env::remove_var("HOME");
        }
        result
    }

    #[test]
    fn rewrites_lab_offload_path_and_strips_runner_flag_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--runner".to_string(),
            "lab-a".to_string(),
            "--json-summary".to_string(),
            "--runner=lab-b".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "lint".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--json-summary".to_string()
            ]
        );
    }

    #[test]
    fn rewrites_equals_path_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path=/Users/chubes/Developer/project".to_string(),
            "--runner=lab".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "test".to_string(),
                "--path=/home/chubes/Developer/project".to_string()
            ]
        );
    }

    #[test]
    fn strips_local_output_path_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "audit".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--runner".to_string(),
            "lab".to_string(),
            "--json-summary".to_string(),
            "--output".to_string(),
            "/var/folders/local/homeboy-audit.json".to_string(),
            "--output=/tmp/other-local-output.json".to_string(),
        ];

        let rewritten = rewrite_lab_offload_args(&args, "/home/chubes/Developer/project");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--json-summary".to_string(),
            ]
        );
        assert!(!rewritten.iter().any(|arg| arg.contains("/var/folders")));
        assert!(!rewritten
            .iter()
            .any(|arg| arg.contains("/tmp/other-local-output.json")));
    }

    #[test]
    fn leaves_passthrough_path_args_untouched() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--".to_string(),
            "--path".to_string(),
            "test-fixture".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "test".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--".to_string(),
                "--path".to_string(),
                "test-fixture".to_string()
            ]
        );
    }

    #[test]
    fn detects_lab_offload_source_path_from_path_flag() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
        ];

        assert_eq!(
            lab_offload_source_path(&args).expect("path"),
            std::path::PathBuf::from("/Users/chubes/Developer/project")
        );
    }

    #[test]
    fn lab_offload_test_preflight_skips_component_script_components() {
        with_temp_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            std::fs::write(
                dir.path().join("homeboy.json"),
                r#"{"id":"fixture","scripts":{"test":["printf ok\n"]},"extensions":{"missing-runner-extension":{}}}"#,
            )
            .expect("write component config");

            let ids =
                lab_offload_test_extension_ids(&Commands::Test(test_args_for_path(dir.path())))
                    .expect("component script should not require extension parity preflight");

            assert!(ids.is_empty());
        });
    }

    #[test]
    fn lab_offload_test_preflight_resolves_selected_test_extension() {
        with_temp_home(|home| {
            let extension_dir = home
                .join(".config")
                .join("homeboy")
                .join("extensions")
                .join("fixture-extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join("fixture-extension.json"),
                r#"{"name":"Fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
            )
            .expect("write extension manifest");

            let dir = tempfile::tempdir().expect("component dir");
            std::fs::write(
                dir.path().join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"fixture-extension":{}}}"#,
            )
            .expect("write component config");

            let ids =
                lab_offload_test_extension_ids(&Commands::Test(test_args_for_path(dir.path())))
                    .expect("test extension should resolve locally before runner parity check");

            assert_eq!(ids, vec!["fixture-extension".to_string()]);
        });
    }

    #[test]
    fn runner_extension_preflight_tail_prefers_recent_diagnostics() {
        let tail = runner_extension_preflight_tail(
            "one\ntwo\nthree\nfour",
            "stdout should be ignored when stderr has enough lines",
        );

        assert!(tail.contains("two\nthree\nfour"));
        assert!(!tail.contains("one"));
    }
}
