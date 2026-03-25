//! helpers — extracted from main.rs.

use clap::{ArgMatches, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use commands::GlobalArgs;
use commands::utils::{args, entity_suggest, response as output, tty};
use commands::{
    api, audit, auth, build, changelog, changes, cli, component, config, db, deploy, extension,
    file, fleet, git, init, lint, logs, project, refactor, release, server, ssh, status, test,
    transfer, undo, upgrade, validate, version,
};
use super::RawOutputMode;
use super::ResponseMode;
use super::Cli;
use super::ExtensionCliInfo;
use super::Commands;


pub(crate) fn response_mode(command: &Commands) -> ResponseMode {
    match command {
        Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_empty() => {
            ResponseMode::Raw(RawOutputMode::InteractivePassthrough)
        }
        Commands::Logs(args) if logs::is_interactive(args) => {
            ResponseMode::Raw(RawOutputMode::InteractivePassthrough)
        }
        Commands::File(args) if file::is_raw_read(args) => {
            ResponseMode::Raw(RawOutputMode::PlainText)
        }
        Commands::Docs(args) if crate::commands::docs::is_json_mode(args) => ResponseMode::Json,
        Commands::Docs(_) => ResponseMode::Raw(RawOutputMode::Markdown),
        Commands::Changelog(args) if changelog::is_show_markdown(args) => {
            ResponseMode::Raw(RawOutputMode::Markdown)
        }
        Commands::List => ResponseMode::Raw(RawOutputMode::Markdown),
        _ => ResponseMode::Json,
    }
}

pub(crate) fn build_augmented_command(extension_info: &[ExtensionCliInfo]) -> Command {
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

pub(crate) fn main() -> std::process::ExitCode {
    let extension_info = collect_extension_cli_info();
    let cmd = build_augmented_command(&extension_info);

    let args: Vec<String> = std::env::args().collect();
    let normalized = args::normalize(args);

    let matches = match cmd.try_get_matches_from(normalized) {
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
    let output_file: Option<String> = matches.get_one::<String>("output").cloned();

    if let Some(extension_cmd) = try_parse_extension_cli_command(&matches, &extension_info) {
        let cli_args = cli::CliArgs {
            tool: extension_cmd.tool,
            identifier: extension_cmd.project_id,
            args: extension_cmd.args,
        };
        let result = cli::run(cli_args, &global);

        let (json_result, exit_code) = output::map_cmd_result_to_json(result);
        if let Some(ref path) = output_file {
            output::write_json_to_file(&json_result, path, exit_code);
        }
        output::print_json_result(json_result, exit_code).ok();
        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
    }

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    // Startup update checks — skip for upgrade/update commands (they handle this themselves)
    if !matches!(&cli.command, Commands::Upgrade(_) | Commands::Update(_)) {
        homeboy::upgrade::update_check::run_startup_check();
        homeboy::extension::update_check::run_startup_check();
    }

    let mode = response_mode(&cli.command);

    match mode {
        ResponseMode::Json => {}
        ResponseMode::Raw(RawOutputMode::InteractivePassthrough) => {
            if !tty::require_tty_for_interactive() {
                let err = homeboy::Error::validation_invalid_argument(
                    "tty",
                    "This command requires an interactive TTY. For non-interactive usage, run: homeboy ssh <target> -- <command...>",
                    None,
                    None,
                );
                output::print_result::<serde_json::Value>(Err(err)).ok();
                return std::process::ExitCode::from(exit_code_to_u8(2));
            }
        }
        ResponseMode::Raw(RawOutputMode::Markdown) => {}
        ResponseMode::Raw(RawOutputMode::PlainText) => {}
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

    if let ResponseMode::Raw(RawOutputMode::Markdown) = mode {
        let markdown_result = commands::run_markdown(cli.command, &global);

        match markdown_result {
            Ok((content, exit_code)) => {
                print!("{}", content);
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            Err(err) => {
                output::print_result::<serde_json::Value>(Err(err)).ok();
                return std::process::ExitCode::from(exit_code_to_u8(1));
            }
        }
    }

    if let ResponseMode::Raw(RawOutputMode::PlainText) = mode {
        if let crate::Commands::File(args) = cli.command {
            let result = file::run(args, &global);
            match result {
                Ok((file::FileCommandOutput::Raw(content), exit_code)) => {
                    print!("{}", content);
                    return std::process::ExitCode::from(exit_code_to_u8(exit_code));
                }
                Ok(_) => {
                    let err =
                        homeboy::Error::internal_unexpected("Unexpected output type for raw mode");
                    output::print_result::<serde_json::Value>(Err(err)).ok();
                    return std::process::ExitCode::from(exit_code_to_u8(1));
                }
                Err(err) => {
                    output::print_result::<serde_json::Value>(Err(err)).ok();
                    return std::process::ExitCode::from(exit_code_to_u8(1));
                }
            }
        }
    }

    let (json_result, exit_code) = commands::run_json(cli.command, &global);

    // Write JSON to --output file if specified (before printing to stdout).
    // The file always gets written, even on failure, so consumers can read
    // structured error data instead of scraping log output.
    if let Some(ref path) = output_file {
        output::write_json_to_file(&json_result, path, exit_code);
    }

    match mode {
        ResponseMode::Json => {
            output::print_json_result(json_result, exit_code).ok();
        }
        ResponseMode::Raw(RawOutputMode::InteractivePassthrough) => {}
        ResponseMode::Raw(RawOutputMode::Markdown) => {}
        ResponseMode::Raw(RawOutputMode::PlainText) => {}
    }

    std::process::ExitCode::from(exit_code_to_u8(exit_code))
}

pub(crate) fn exit_code_to_u8(code: i32) -> u8 {
    if code <= 0 {
        0
    } else if code >= 255 {
        255
    } else {
        code as u8
    }
}
