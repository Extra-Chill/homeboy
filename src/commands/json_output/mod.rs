use serde_json::Value;

use crate::cli_surface::Commands;

use super::GlobalArgs;

mod ops;
mod quality;
mod workspace;

type JsonRun = (homeboy::core::Result<Value>, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonCommandFamily {
    Quality,
    Workspace,
    Ops,
    RawOnly,
}

/// Dispatch a command to its handler and map the structured result to JSON.
pub fn run(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    crate::commands::utils::tty::status("homeboy is working...");

    dispatch(command, global)
}

fn dispatch(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    match family(&command) {
        JsonCommandFamily::Quality => quality::dispatch(command, global),
        JsonCommandFamily::Workspace => workspace::dispatch(command, global),
        JsonCommandFamily::Ops => ops::dispatch(command, global),
        JsonCommandFamily::RawOnly => unsupported_raw_command("List command uses raw output mode"),
    }
}

fn family(command: &Commands) -> JsonCommandFamily {
    match command {
        Commands::Test(_)
        | Commands::Bench(_)
        | Commands::Trace(_)
        | Commands::Observe(_)
        | Commands::Lint(_)
        | Commands::Review(_)
        | Commands::Audit(_) => JsonCommandFamily::Quality,
        Commands::Project(_)
        | Commands::Component(_)
        | Commands::Config(_)
        | Commands::Extension(_)
        | Commands::Docs(_)
        | Commands::Changelog(_)
        | Commands::Version(_)
        | Commands::Build(_)
        | Commands::Changes(_)
        | Commands::Release(_)
        | Commands::Report(_)
        | Commands::Refactor(_)
        | Commands::Rig(_)
        | Commands::Runner(_)
        | Commands::Runs(_)
        | Commands::Stack(_)
        | Commands::Undo(_) => JsonCommandFamily::Workspace,
        Commands::Status(_)
        | Commands::Ci(_)
        | Commands::Ssh(_)
        | Commands::Server(_)
        | Commands::Db(_)
        | Commands::Deps(_)
        | Commands::Doctor(_)
        | Commands::File(_)
        | Commands::Fleet(_)
        | Commands::Logs(_)
        | Commands::Triage(_)
        | Commands::Deploy(_)
        | Commands::Daemon(_)
        | Commands::Git(_)
        | Commands::Issues(_)
        | Commands::SelfCmd(_)
        | Commands::Auth(_)
        | Commands::Api(_)
        | Commands::Http(_)
        | Commands::Upgrade(_) => JsonCommandFamily::Ops,
        Commands::List => JsonCommandFamily::RawOnly,
    }
}

fn map<T: serde::Serialize>(result: super::CmdResult<T>) -> JsonRun {
    crate::commands::utils::response::map_cmd_result_to_json(result)
}

fn unsupported_raw_command(message: &'static str) -> JsonRun {
    let err = homeboy::core::Error::validation_invalid_argument("output_mode", message, None, None);
    crate::commands::utils::response::map_cmd_result_to_json::<Value>(Err(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_dispatch_reports_raw_output_mode() {
        let (result, exit_code) = dispatch(Commands::List, &GlobalArgs {});

        assert_ne!(exit_code, 0);
        assert!(result
            .expect_err("list should not dispatch as JSON")
            .to_string()
            .contains("raw output mode"));
    }
}
