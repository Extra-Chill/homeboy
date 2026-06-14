use serde_json::Value;

use crate::command_contract::{
    CommandDescriptor, CommandJsonFamily, CommandOutputContractKind, CommandOutputFileMode,
    CommandRawOutputMode, CommandResponseMode,
};

use super::GlobalArgs;

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
    pub response_mode: CommandResponseMode,
    pub output_file_mode: CommandOutputFileMode,
    pub json_family: CommandJsonFamily,
    pub output_contract: CommandOutputContractKind,
    pub lab_runner: CommandLabRunnerPolicy,
}

impl CommandAdapterContract {
    pub fn to_descriptor(self) -> CommandDescriptor {
        CommandDescriptor {
            response_mode: self.response_mode,
            output_file_mode: self.output_file_mode,
            json_family: self.json_family,
            supports_lab_runner: self.lab_runner.supports_runner,
            lab_runner_unsupported_reason: self.lab_runner.unsupported_reason,
            lab_offload_mutation_flag: self.lab_runner.mutation_flag,
            output_contract: self.output_contract,
        }
    }
}

pub(crate) struct TypedCommandAdapter<Args> {
    pub contract: CommandAdapterContract,
    pub execute_json: Option<JsonCommandExecutor<Args>>,
}

impl<Args> TypedCommandAdapter<Args> {
    pub fn json_only(
        json_family: CommandJsonFamily,
        output_file_mode: CommandOutputFileMode,
        output_contract: CommandOutputContractKind,
        execute_json: JsonCommandExecutor<Args>,
    ) -> Self {
        Self {
            contract: CommandAdapterContract {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family,
                output_contract,
                lab_runner: CommandLabRunnerPolicy::LOCAL,
            },
            execute_json: Some(execute_json),
        }
    }

    #[allow(dead_code)]
    pub fn raw_only(
        raw_mode: CommandRawOutputMode,
        json_family: CommandJsonFamily,
        output_file_mode: CommandOutputFileMode,
        output_contract: CommandOutputContractKind,
    ) -> Self {
        Self {
            contract: CommandAdapterContract {
                response_mode: CommandResponseMode::Raw(raw_mode),
                output_file_mode,
                json_family,
                output_contract,
                lab_runner: CommandLabRunnerPolicy::LOCAL,
            },
            execute_json: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_only_contract_maps_to_descriptor() {
        let adapter = TypedCommandAdapter::<()>::json_only(
            CommandJsonFamily::Workspace,
            CommandOutputFileMode::GenericEnvelope,
            CommandOutputContractKind::JsonEnvelope,
            |_, _| (Ok(Value::Null), 0),
        );

        let descriptor = adapter.contract.to_descriptor();

        assert_eq!(descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(
            descriptor.output_file_mode,
            CommandOutputFileMode::GenericEnvelope
        );
        assert_eq!(descriptor.json_family, CommandJsonFamily::Workspace);
        assert!(!descriptor.supports_lab_runner);
        assert!(adapter.execute_json.is_some());
    }

    #[test]
    fn raw_contract_can_express_supported_raw_modes() {
        for raw_mode in [
            CommandRawOutputMode::Markdown,
            CommandRawOutputMode::PlainText,
            CommandRawOutputMode::InteractivePassthrough,
        ] {
            let adapter = TypedCommandAdapter::<()>::raw_only(
                raw_mode,
                CommandJsonFamily::RawOnly,
                CommandOutputFileMode::None,
                CommandOutputContractKind::RawOnly,
            );

            assert_eq!(
                adapter.contract.to_descriptor().response_mode,
                CommandResponseMode::Raw(raw_mode)
            );
            assert!(adapter.execute_json.is_none());
        }
    }
}
