use serde_json::Value;

use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, CommandPortabilityContract,
    LabCommandContract,
};

use crate::cli_surface::Commands;

use super::{fleet, observe, version, GlobalArgs};

pub(crate) type JsonCommandRun = (homeboy::core::Result<Value>, i32);
pub(crate) type JsonCommandExecutor<Args> = fn(Args, &GlobalArgs) -> JsonCommandRun;
pub(crate) type LabContractResolver<Args> = fn(&Args) -> Option<LabCommandContract>;
pub(crate) type PortabilityContractResolver<Args> = fn(&Args) -> CommandPortabilityContract;

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
    pub portability_contract: Option<PortabilityContractResolver<Args>>,
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
            lab_contract: None,
            portability_contract: None,
        }
    }

    pub fn with_lab_contract(mut self, lab_contract: LabContractResolver<Args>) -> Self {
        self.lab_contract = Some(lab_contract);
        self
    }

    pub fn with_portability_contract(
        mut self,
        portability_contract: PortabilityContractResolver<Args>,
    ) -> Self {
        self.portability_contract = Some(portability_contract);
        self
    }

    pub fn portability_contract(&self, args: &Args) -> CommandPortabilityContract {
        if let Some(resolver) = self.portability_contract {
            return resolver(args);
        }
        CommandPortabilityContract::lab_optional(
            self.lab_contract.and_then(|resolver| resolver(args)),
        )
    }

    pub fn lab_contract(&self, args: &Args) -> Option<LabCommandContract> {
        self.portability_contract(args).lab_command()
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
        Commands::Observe(args) => {
            let executor = observe::adapter(output_file_mode)
                .execute_json
                .expect("observe adapter supports JSON execution");
            Ok(BoundCommandAdapter::bind(args, executor))
        }
        command => Err(command),
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
            parsed_command(&["homeboy", "version", "show"]),
            CommandOutputFileMode::None,
        )
        .is_ok());
        assert!(command_adapter(
            parsed_command(&["homeboy", "observe", "demo", "--watch-process", "sleep"]),
            CommandOutputFileMode::None,
        )
        .is_ok());

        assert!(command_adapter(
            Commands::Manifest(crate::commands::manifest::ManifestArgs {}),
            CommandOutputFileMode::None,
        )
        .is_ok());
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
    fn adapter_can_expose_generic_portability_contract() {
        let adapter = TypedCommandAdapter::<()>::json_only(
            CommandJsonFamily::Quality,
            CommandOutputFileMode::None,
            |_, _| (Ok(Value::Null), 0),
        )
        .with_portability_contract(|_| {
            CommandPortabilityContract::lab(LabCommandContract::portable(
                "adapter-owned",
                None,
                false,
                &[],
            ))
        });

        let contract = adapter
            .portability_contract(&())
            .lab_command()
            .expect("adapter should expose Lab portability");

        assert_eq!(contract.hot_label, "adapter-owned");
        assert!(matches!(
            contract.portability,
            LabCommandPortability::Portable
        ));
    }
}
