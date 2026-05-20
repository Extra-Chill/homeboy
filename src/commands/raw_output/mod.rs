use crate::cli_surface::{CommandRawOutputMode, Commands};

use super::{changelog, docs, file, report, review, runs, trace, GlobalArgs};

pub fn run(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandRawOutputMode,
) -> Option<homeboy::core::Result<(String, i32)>> {
    match mode {
        CommandRawOutputMode::InteractivePassthrough => None,
        CommandRawOutputMode::Markdown => Some(run_markdown(command, global)),
        CommandRawOutputMode::PlainText => Some(run_plain_text(command, global)),
    }
}

fn run_markdown(command: Commands, global: &GlobalArgs) -> homeboy::core::Result<(String, i32)> {
    match command {
        Commands::Docs(args) => docs::run_markdown(args),
        Commands::Changelog(args) => changelog::run_markdown(args),
        Commands::Review(args) => review::run_markdown(args, global),
        Commands::Trace(args) => trace::run_markdown(args, global),
        Commands::Runs(args) => runs::run_markdown(args, global),
        Commands::Report(args) => report::run_markdown(args),
        _ => unsupported_output("markdown"),
    }
}

fn run_plain_text(command: Commands, global: &GlobalArgs) -> homeboy::core::Result<(String, i32)> {
    match command {
        Commands::File(args) => match file::run(args, global)? {
            (file::FileCommandOutput::Raw(content), exit_code) => Ok((content, exit_code)),
            _ => Err(homeboy::core::Error::internal_unexpected(
                "Unexpected output type for raw mode",
            )),
        },
        _ => unsupported_output("plain text"),
    }
}

fn unsupported_output(mode: &str) -> homeboy::core::Result<(String, i32)> {
    Err(homeboy::core::Error::validation_invalid_argument(
        "output_mode",
        format!("Command does not support {mode} output"),
        None,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_passthrough_has_no_raw_text_result() {
        let result = run(
            Commands::List,
            &GlobalArgs {},
            CommandRawOutputMode::InteractivePassthrough,
        );

        assert!(result.is_none());
    }

    #[test]
    fn unsupported_plain_text_command_returns_output_mode_error() {
        let result = run(
            Commands::List,
            &GlobalArgs {},
            CommandRawOutputMode::PlainText,
        )
        .expect("plain text mode should return a result")
        .expect_err("list does not support plain text output");

        assert!(result.to_string().contains("plain text output"));
    }
}
