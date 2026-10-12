use serde_json::Value;

use crate::command_contract::{CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode};

use crate::cli_surface::Commands;

use crate::commands::contract;

pub(crate) type JsonHandlerResult = (homeboy::core::Result<Value>, i32);
pub(crate) type JsonCommandExecutor<Args> = fn(Args) -> JsonHandlerResult;
type CommandAdapterResult = Result<BoundCommandAdapter, Box<Commands>>;

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

/// Adapter-owned metadata for a parsed command family.
///
/// New command migrations should keep output contract, JSON dispatch, raw
/// dispatch hooks, and Lab policy together in the command module's adapter. The
/// top-level dispatch modules should only bind the parsed enum variant to that
/// adapter, so follow-up PRs can migrate one family at a time without changing
/// public CLI behavior.
pub(crate) struct TypedCommandAdapter<Args> {
    pub contract: CommandAdapterContract,
    pub execute_json: Option<JsonCommandExecutor<Args>>,
}

pub(crate) struct BoundCommandAdapter {
    run: Box<dyn FnOnce() -> JsonHandlerResult>,
}

impl BoundCommandAdapter {
    /// Bind already-parsed arguments to their typed executor, capturing both in a
    /// single delegation closure. This keeps the adapter thin: every command's
    /// real work lives behind its own `execute_json` executor, and binding adds
    /// no per-command dispatch arms here — only argument-to-executor pairing.
    fn bind<Args: 'static>(args: Args, executor: JsonCommandExecutor<Args>) -> Self {
        Self {
            run: Box::new(move || executor(args)),
        }
    }

    pub fn run(self) -> JsonHandlerResult {
        (self.run)()
    }
}

impl<Args> TypedCommandAdapter<Args> {
    pub(crate) fn output_descriptor(&self) -> CommandOutputDescriptor {
        self.contract.to_output_descriptor()
    }

    pub(crate) fn json_only(
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
) -> CommandAdapterResult {
    match command {
        Commands::Contract(args) => {
            let executor = contract::adapter(output_file_mode)
                .execute_json
                .expect("contract adapter supports JSON execution");
            Ok(BoundCommandAdapter::bind(args, executor))
        }
        command => Err(Box::new(command)),
    }
}

pub(crate) fn output_descriptor(
    command: &Commands,
    output_file_mode: CommandOutputFileMode,
) -> Option<CommandOutputDescriptor> {
    match command {
        Commands::Contract(_) => Some(contract::adapter(output_file_mode).output_descriptor()),
        _ => None,
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
            |_| (Ok(Value::Null), 0),
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
            parsed_command(&["homeboy", "contract", "manifest"]),
            CommandOutputFileMode::None,
        )
        .is_ok());
    }

    #[test]
    fn command_adapter_executes_migrated_command_with_unchanged_json_result() {
        let Ok(adapter) = command_adapter(
            parsed_command(&["homeboy", "contract", "manifest"]),
            CommandOutputFileMode::None,
        ) else {
            panic!("contract manifest is adapter-backed");
        };

        let (result, exit_code) = adapter.run();

        assert_eq!(exit_code, 0);
        let json = result.expect("contract manifest JSON result");
        assert_eq!(json["command"], "contract.manifest");
        assert!(json["commands"].is_array());
    }

    #[test]
    fn command_adapter_preserves_unhandled_command_for_existing_dispatch_diagnostics() {
        let Err(command) = command_adapter(
            parsed_command(&["homeboy", "status"]),
            CommandOutputFileMode::None,
        ) else {
            panic!("status remains owned by the existing workspace dispatcher");
        };

        assert!(matches!(*command, Commands::Status(_)));
    }

    #[test]
    fn migrated_adapter_output_descriptor_uses_adapter_contract() {
        let descriptor = output_descriptor(
            &parsed_command(&["homeboy", "contract", "manifest"]),
            CommandOutputFileMode::GenericEnvelope,
        )
        .expect("contract is adapter-backed");

        assert_eq!(descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(descriptor.json_family, CommandJsonFamily::Workspace);
        assert_eq!(
            descriptor.output_file_mode,
            CommandOutputFileMode::GenericEnvelope
        );
        assert_eq!(
            descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );
    }

    #[test]
    fn migrated_adapter_output_descriptors_match_command_contracts() {
        let commands = [(
            "contract",
            parsed_command(&["homeboy", "contract", "manifest"]),
        )];
        let output_file_modes = [
            CommandOutputFileMode::None,
            CommandOutputFileMode::GenericEnvelope,
        ];

        for (name, command) in commands {
            let spec = crate::command_contract::registered_command(name).unwrap();
            for output_file_mode in output_file_modes {
                let has_output_file = output_file_mode != CommandOutputFileMode::None;

                assert_eq!(
                    output_descriptor(&command, output_file_mode),
                    Some(command.output_descriptor(spec, has_output_file))
                );
            }
        }
    }

    #[test]
    fn trace_observe_output_descriptor_matches_registered_command_contract() {
        let command = parsed_command(&[
            "homeboy",
            "trace",
            "observe",
            "demo",
            "--watch-process",
            "sleep",
        ]);
        let spec = crate::command_contract::registered_command("trace").unwrap();

        assert_eq!(
            command.output_descriptor(spec, false),
            spec.output_descriptor(CommandOutputFileMode::None)
        );
    }
}
