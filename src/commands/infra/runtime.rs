use clap::{Args, Subcommand};
use serde::Serialize;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct RuntimeArgs {
    #[command(subcommand)]
    command: RuntimeCommand,
}

#[derive(Subcommand)]
enum RuntimeCommand {
    /// Inspect core-bundled runtime helper paths exposed to extension runners.
    Helper {
        #[command(subcommand)]
        command: RuntimeHelperCommand,
    },
}

#[derive(Subcommand)]
enum RuntimeHelperCommand {
    /// Print the materialized path for a known core runtime helper.
    Path {
        /// Print only the path, for shell bootstrap usage.
        #[arg(long)]
        plain: bool,

        /// Known helper filename or injected HOMEBOY_RUNTIME_* env var name.
        helper: String,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum RuntimeOutput {
    HelperPath(RuntimeHelperPathOutput),
}

#[derive(Serialize)]
pub struct RuntimeHelperPathOutput {
    command: String,
    helper: String,
    path: String,
}

pub fn run(args: RuntimeArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<RuntimeOutput> {
    match args.command {
        RuntimeCommand::Helper { command } => match command {
            RuntimeHelperCommand::Path { helper, .. } => helper_path(&helper),
        },
    }
}

pub fn is_plain_mode(args: &RuntimeArgs) -> bool {
    match &args.command {
        RuntimeCommand::Helper { command } => match command {
            RuntimeHelperCommand::Path { plain, .. } => *plain,
        },
    }
}

pub fn run_plain_text(args: RuntimeArgs) -> homeboy::core::Result<(String, i32)> {
    match args.command {
        RuntimeCommand::Helper { command } => match command {
            RuntimeHelperCommand::Path { helper, .. } => {
                let path = homeboy::core::extension::helper_path(&helper)?;
                Ok((format!("{}\n", path.to_string_lossy()), 0))
            }
        },
    }
}

fn helper_path(helper: &str) -> CmdResult<RuntimeOutput> {
    let path = homeboy::core::extension::helper_path(helper)?;

    Ok((
        RuntimeOutput::HelperPath(RuntimeHelperPathOutput {
            command: "runtime.helper.path".to_string(),
            helper: helper.to_string(),
            path: path.to_string_lossy().to_string(),
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_path_resolves_core_helper() {
        crate::test_support::with_isolated_home(|_| {
            let (output, exit_code) = helper_path("command-capture.sh").unwrap();

            assert_eq!(exit_code, 0);
            let RuntimeOutput::HelperPath(output) = output;
            assert!(output.path.ends_with("command-capture.sh"));
            assert!(std::path::Path::new(&output.path).is_file());
        });
    }

    #[test]
    fn helper_path_plain_prints_only_path() {
        crate::test_support::with_isolated_home(|_| {
            let args = RuntimeArgs {
                command: RuntimeCommand::Helper {
                    command: RuntimeHelperCommand::Path {
                        plain: true,
                        helper: "runner-prelude.sh".to_string(),
                    },
                },
            };

            let (output, exit_code) = run_plain_text(args).unwrap();

            assert_eq!(exit_code, 0);
            assert!(output.ends_with("runner-prelude.sh\n"));
            assert!(std::path::Path::new(output.trim()).is_file());
        });
    }
}
