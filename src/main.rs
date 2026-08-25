use clap::{ArgMatches, Command};
use homeboy::cli_runtime::{CliCapability, CliRuntime};

static PRODUCT_CAPABILITIES: [&'static dyn homeboy::cli_runtime::CliCapability; 1] =
    [&TRIAGE_CAPABILITY];

struct TriageCapability;

static TRIAGE_CAPABILITY: TriageCapability = TriageCapability;

impl CliCapability for TriageCapability {
    fn name(&self) -> &'static str {
        homeboy_triage::COMMAND_NAME
    }

    fn command(&self) -> Command {
        homeboy_triage::command()
    }

    fn run(&self, matches: &ArgMatches) -> homeboy::core::Result<(serde_json::Value, i32)> {
        homeboy_triage::run_command(matches)
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("__homeboy_status_probe") {
        return homeboy::commands::status::run_status_probe_child(args.get(2));
    }
    let runtime = CliRuntime::try_with_required_capabilities(
        &PRODUCT_CAPABILITIES,
        &[homeboy_triage::COMMAND_NAME],
    )
    .expect("standard product capability composition must be complete");
    if let Some(exit_code) = runtime.run_startup_fast_path(&args) {
        return exit_code;
    }
    runtime.run_from_args(args)
}
