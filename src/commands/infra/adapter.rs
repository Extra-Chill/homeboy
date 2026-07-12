use serde_json::Value;

use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
};

use crate::cli_surface::Commands;

use crate::commands::output_runtime::CommandRun;
use crate::commands::{contract, fleet, observe, GlobalArgs};

pub(crate) type JsonHandlerResult = (homeboy::core::Result<Value>, i32);
pub(crate) type JsonCommandExecutor<Args> = fn(Args, &GlobalArgs) -> JsonHandlerResult;
pub(crate) type LabContractResolver<Args> = fn(&Args) -> Option<LabCommandContract>;

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
    pub lab_contract: Option<LabContractResolver<Args>>,
}

pub(crate) struct BoundCommandAdapter {
    run: Box<dyn FnOnce(&GlobalArgs) -> JsonHandlerResult>,
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

    pub fn run(self, global: &GlobalArgs) -> JsonHandlerResult {
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
            lab_contract: None,
        }
    }

    pub fn with_lab_contract(mut self, lab_contract: LabContractResolver<Args>) -> Self {
        self.lab_contract = Some(lab_contract);
        self
    }

    pub fn lab_contract(&self, args: &Args) -> Option<LabCommandContract> {
        self.lab_contract.and_then(|resolver| resolver(args))
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
        Commands::Observe(args) => {
            let executor = observe::adapter(output_file_mode)
                .execute_json
                .expect("observe adapter supports JSON execution");
            Ok(BoundCommandAdapter::bind(args, executor))
        }
        Commands::Contract(args) => {
            let executor = contract::adapter(output_file_mode)
                .execute_json
                .expect("contract adapter supports JSON execution");
            Ok(BoundCommandAdapter::bind(args, executor))
        }
        command => Err(command),
    }
}

pub(crate) fn run_command_output(
    command: Commands,
    command_name: &'static str,
    global: &GlobalArgs,
    output_file_mode: CommandOutputFileMode,
) -> Result<CommandRun, Commands> {
    let (stdout_result, exit_code) = run_json_output(command, global, output_file_mode)?;

    Ok(CommandRun::from_command_stdout_result(
        command_name,
        stdout_result,
        exit_code,
    ))
}

pub(crate) fn run_json_output(
    command: Commands,
    global: &GlobalArgs,
    output_file_mode: CommandOutputFileMode,
) -> Result<JsonHandlerResult, Commands> {
    let adapter = command_adapter(command, output_file_mode)?;
    Ok(adapter.run(global))
}

pub(crate) fn output_descriptor(
    command: &Commands,
    output_file_mode: CommandOutputFileMode,
) -> Option<CommandOutputDescriptor> {
    match command {
        Commands::Fleet(_) => Some(fleet::adapter(output_file_mode).output_descriptor()),
        Commands::Observe(_) => Some(observe::adapter(output_file_mode).output_descriptor()),
        Commands::Contract(_) => Some(contract::adapter(output_file_mode).output_descriptor()),
        _ => None,
    }
}

pub(crate) fn lab_contract(command: &Commands) -> Option<LabCommandContract> {
    match command {
        Commands::Fleet(args) => fleet::adapter(CommandOutputFileMode::None).lab_contract(args),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_contract::{
        CommandOutputContractKind, CommandResponseMode, LabCommandPortability,
    };
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
            parsed_command(&["homeboy", "fleet", "list"]),
            CommandOutputFileMode::None,
        )
        .is_ok());

        assert!(command_adapter(
            parsed_command(&["homeboy", "observe", "demo", "--watch-process", "sleep"]),
            CommandOutputFileMode::None,
        )
        .is_ok());

        assert!(command_adapter(
            parsed_command(&["homeboy", "contract", "manifest"]),
            CommandOutputFileMode::None,
        )
        .is_ok());
    }

    #[test]
    fn run_command_output_routes_migrated_json_command() {
        let run = run_command_output(
            parsed_command(&["homeboy", "contract", "manifest"]),
            "contract",
            &GlobalArgs {},
            CommandOutputFileMode::None,
        )
        .unwrap_or_else(|_| panic!("contract should route through adapter JSON output"));

        assert_eq!(run.command, "contract");
        assert_eq!(run.exit_code, 0);
        let value = run.stdout_result.expect("manifest should dispatch as JSON");
        assert_eq!(value["command"], "contract.manifest");
        assert!(value["commands"].is_array());
    }

    #[test]
    fn run_json_output_matches_adapter_bound_execution() {
        let command = parsed_command(&["homeboy", "contract", "manifest"]);
        let (adapter_stdout, adapter_exit_code) = command_adapter(
            parsed_command(&["homeboy", "contract", "manifest"]),
            CommandOutputFileMode::None,
        )
        .unwrap_or_else(|_| panic!("contract should bind an adapter"))
        .run(&GlobalArgs {});

        let (helper_stdout, helper_exit_code) =
            run_json_output(command, &GlobalArgs {}, CommandOutputFileMode::None)
                .unwrap_or_else(|_| panic!("contract should route through adapter helper"));

        assert_eq!(helper_exit_code, adapter_exit_code);
        assert_eq!(helper_stdout.unwrap(), adapter_stdout.unwrap());
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
        let commands = [
            ("fleet", parsed_command(&["homeboy", "fleet", "list"])),
            (
                "observe",
                parsed_command(&["homeboy", "observe", "demo", "--watch-process", "sleep"]),
            ),
            (
                "contract",
                parsed_command(&["homeboy", "contract", "manifest"]),
            ),
        ];
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
    fn fleet_adapter_owns_hot_exec_lab_contract() {
        let Commands::Fleet(args) = parsed_command(&[
            "homeboy", "fleet", "exec", "--apply", "growth", "wp", "plugin", "list",
        ]) else {
            panic!("expected parsed fleet command");
        };

        let contract = fleet::adapter(CommandOutputFileMode::None)
            .lab_contract(&args)
            .expect("hot fleet exec should declare a Lab contract");

        assert_eq!(contract.hot_label, "fleet exec");
        assert!(matches!(
            contract.portability,
            LabCommandPortability::LocalOnly(reason)
                if reason.contains("runner-side config parity")
        ));
    }

    #[test]
    fn fleet_adapter_leaves_cold_commands_without_lab_contract() {
        let Commands::Fleet(args) = parsed_command(&["homeboy", "fleet", "list"]) else {
            panic!("expected parsed fleet command");
        };

        assert!(fleet::adapter(CommandOutputFileMode::None)
            .lab_contract(&args)
            .is_none());
    }

    #[test]
    fn migrated_adapter_lab_contract_matches_command_contract() {
        let command = parsed_command(&[
            "homeboy", "fleet", "exec", "--apply", "growth", "wp", "plugin", "list",
        ]);

        assert_eq!(lab_contract(&command), command.lab_contract());
    }
}
