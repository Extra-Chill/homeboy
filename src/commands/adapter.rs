use serde_json::Value;

use crate::command_contract::{CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode};

use crate::cli_surface::Commands;

use super::{fleet, version, GlobalArgs};

pub(crate) type JsonCommandRun = (homeboy::core::Result<Value>, i32);
pub(crate) type JsonCommandExecutor<Args> = fn(Args, &GlobalArgs) -> JsonCommandRun;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandLabRunnerPolicy {
    pub supports_runner: bool,
    pub unsupported_reason: Option<&'static str>,
    pub mutation_flag: Option<&'static str>,
}

impl CommandLabRunnerPolicy {
    pub const LOCAL: Self = Self {
        supports_runner: false,
        unsupported_reason: None,
        mutation_flag: None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandAdapterContract {
    /// Shared output-routing spec (response mode, output-file mode, JSON family,
    /// output contract), reused from [`CommandOutputDescriptor`] so the field
    /// group is declared once.
    pub output: CommandOutputDescriptor,
    pub lab_runner: CommandLabRunnerPolicy,
}

impl CommandAdapterContract {
    fn to_output_descriptor(self) -> CommandOutputDescriptor {
        self.output
    }
}

pub(crate) struct TypedCommandAdapter<Args> {
    pub contract: CommandAdapterContract,
    pub execute_json: Option<JsonCommandExecutor<Args>>,
}

pub(crate) struct BoundCommandAdapter {
    run: Box<dyn FnOnce(&GlobalArgs) -> JsonCommandRun>,
}

impl BoundCommandAdapter {
    /// Bind already-parsed arguments to their typed executor, capturing both in a
    /// single delegation closure. This keeps the adapter thin: every command's
    /// real work lives behind its own `execute_json` executor, and binding adds
    /// no per-command dispatch arms here — only argument-to-executor pairing.
    fn bind<Args: 'static>(args: Args, executor: JsonCommandExecutor<Args>) -> Self {
        Self {
            run: Box::new(move |global| executor(args, global)),
        }
    }

    pub fn run(self, global: &GlobalArgs) -> JsonCommandRun {
        (self.run)(global)
    }
}

impl<Args> TypedCommandAdapter<Args> {
    pub fn output_descriptor(&self) -> CommandOutputDescriptor {
        self.contract.to_output_descriptor()
    }

    pub fn json_only(
        json_family: CommandJsonFamily,
        output_file_mode: CommandOutputFileMode,
        execute_json: JsonCommandExecutor<Args>,
    ) -> Self {
        Self {
            contract: CommandAdapterContract {
                output: CommandOutputDescriptor::json_envelope(json_family, output_file_mode),
                lab_runner: CommandLabRunnerPolicy::LOCAL,
            },
            execute_json: Some(execute_json),
        }
    }
}

pub(crate) fn command_adapter(
    command: Commands,
    output_file_mode: CommandOutputFileMode,
) -> Result<BoundCommandAdapter, Commands> {
    match command {
        Commands::Fleet(args) => {
            let executor = fleet::adapter(output_file_mode)
                .execute_json
                .expect("fleet adapter supports JSON execution");
            Ok(BoundCommandAdapter::bind(args, executor))
        }
        Commands::Version(args) => {
            let executor = version::adapter(output_file_mode)
                .execute_json
                .expect("version adapter supports JSON execution");
            Ok(BoundCommandAdapter::bind(args, executor))
        }
        command => Err(command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_contract::{CommandOutputContractKind, CommandResponseMode};
    use clap::Parser;

    fn parsed_command(args: &[&str]) -> Commands {
        crate::cli_surface::Cli::try_parse_from(args)
            .expect("CLI args should parse")
            .command
    }

    #[test]
    fn json_only_contract_maps_to_output_descriptor() {
        let adapter = TypedCommandAdapter::<()>::json_only(
            CommandJsonFamily::Workspace,
            CommandOutputFileMode::GenericEnvelope,
            |_, _| (Ok(Value::Null), 0),
        );

        let descriptor = adapter.output_descriptor();

        assert_eq!(descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(
            descriptor.output_file_mode,
            CommandOutputFileMode::GenericEnvelope
        );
        assert_eq!(descriptor.json_family, CommandJsonFamily::Workspace);
        assert_eq!(
            descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );
        assert!(adapter.execute_json.is_some());
    }
    #[test]
    fn command_adapter_recognizes_migrated_json_commands() {
        assert!(command_adapter(
            parsed_command(&["homeboy", "version", "show"]),
            CommandOutputFileMode::None,
        )
        .is_ok());

        assert!(
            command_adapter(Commands::List { json: false }, CommandOutputFileMode::None).is_err()
        );
    }
}
