use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::core::extension::{self, ExtensionCapability};

use super::source_command::resolve_source_context;
use super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
    FUZZ_LAB_LABEL,
};

#[derive(Args)]
pub struct FuzzArgs {
    #[command(subcommand)]
    command: Option<FuzzCommand>,

    #[command(flatten)]
    pub run: FuzzRunArgs,
}

impl FuzzArgs {
    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(CommandJsonFamily::Quality, output_file_mode)
    }

    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        self.is_run_invocation()
            .then(|| LabCommandContract::portable_workload(FUZZ_LAB_LABEL, None, true, &[]))
    }

    pub fn is_run_invocation(&self) -> bool {
        matches!(self.command, None | Some(FuzzCommand::Run(_)))
    }

    pub fn extension_override_ids(&self) -> &[String] {
        self.run.extension_override.extensions.as_slice()
    }
}

#[derive(Subcommand)]
enum FuzzCommand {
    /// List declared fuzz workloads without executing them
    List(FuzzListArgs),
    /// Resolve the selected fuzz workload contract without executing it
    Run(FuzzRunArgs),
}

#[derive(Args, Clone)]
struct FuzzListArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    setting_args: SettingArgs,
}

#[derive(Args, Clone)]
pub struct FuzzRunArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Extension-declared workload id to select.
    #[arg(long = "workload", value_name = "ID")]
    workload_id: Option<String>,

    /// Stable caller-supplied proof label for downstream fuzz runners.
    #[arg(long = "run-id", value_name = "ID")]
    run_id: Option<String>,

    /// Deterministic seed forwarded by future fuzz runners.
    #[arg(long, value_name = "SEED")]
    seed: Option<String>,

    /// Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m.
    #[arg(long, value_name = "DURATION")]
    max_duration: Option<String>,

    /// Additional runner arguments reserved for the fuzz extension script.
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Serialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum FuzzOutput {
    List(FuzzListOutput),
    Run(FuzzRunOutput),
}

#[derive(Serialize)]
pub struct FuzzListOutput {
    pub command: String,
    pub component: String,
    pub workloads: Vec<FuzzWorkloadOutput>,
    pub count: usize,
}

#[derive(Serialize)]
pub struct FuzzRunOutput {
    pub command: String,
    pub component: String,
    pub status: String,
    pub workload_id: Option<String>,
    pub run_id: Option<String>,
    pub seed: Option<String>,
    pub max_duration: Option<String>,
    pub passthrough_args: Vec<String>,
    pub runner_contract: FuzzRunnerContract,
}

#[derive(Clone, Serialize)]
pub struct FuzzWorkloadOutput {
    pub id: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub source: String,
}

#[derive(Serialize)]
pub struct FuzzRunnerContract {
    pub capability: String,
    pub extension_script_required: bool,
    pub env: Vec<&'static str>,
}

pub fn run(args: FuzzArgs, _global: &GlobalArgs) -> CmdResult<FuzzOutput> {
    match args.command {
        Some(FuzzCommand::List(list_args)) => Ok((FuzzOutput::List(run_list(list_args)?), 0)),
        Some(FuzzCommand::Run(run_args)) => Ok((FuzzOutput::Run(run_run(run_args)?), 0)),
        None => Ok((FuzzOutput::Run(run_run(args.run)?), 0)),
    }
}

fn run_list(args: FuzzListArgs) -> homeboy::core::Result<FuzzListOutput> {
    let ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        None,
    )?;
    let workloads = fuzz_workloads(&ctx.component);

    Ok(FuzzListOutput {
        command: "fuzz.list".to_string(),
        component: ctx.component_id,
        count: workloads.len(),
        workloads,
    })
}

fn run_run(args: FuzzRunArgs) -> homeboy::core::Result<FuzzRunOutput> {
    let ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        Some(ExtensionCapability::Fuzz),
    )?;
    let workloads = fuzz_workloads(&ctx.component);
    if let Some(workload_id) = args.workload_id.as_ref() {
        validate_workload_id(&workloads, workload_id)?;
    }

    Ok(FuzzRunOutput {
        command: "fuzz.run".to_string(),
        component: ctx.component_id,
        status: "planned".to_string(),
        workload_id: args.workload_id,
        run_id: args.run_id,
        seed: args.seed,
        max_duration: args.max_duration,
        passthrough_args: args.args,
        runner_contract: FuzzRunnerContract {
            capability: "fuzz".to_string(),
            extension_script_required: true,
            env: vec![
                "HOMEBOY_FUZZ_WORKLOAD_ID",
                "HOMEBOY_FUZZ_RUN_ID",
                "HOMEBOY_FUZZ_SEED",
                "HOMEBOY_FUZZ_MAX_DURATION",
            ],
        },
    })
}

fn fuzz_workloads(component: &homeboy::core::component::Component) -> Vec<FuzzWorkloadOutput> {
    let mut workloads: Vec<FuzzWorkloadOutput> = component
        .script_commands(ExtensionCapability::Fuzz)
        .iter()
        .enumerate()
        .map(|(index, _command)| FuzzWorkloadOutput {
            id: format!("component-script-{}", index + 1),
            label: None,
            description: None,
            source: "component.scripts.fuzz".to_string(),
        })
        .collect();

    if let Some(extensions) = component.extensions.as_ref() {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                workloads.extend(manifest.fuzz_workloads().iter().map(|workload| {
                    FuzzWorkloadOutput {
                        id: workload.id.clone(),
                        label: workload.label.clone(),
                        description: workload.description.clone(),
                        source: format!("extension:{extension_id}"),
                    }
                }));
            }
        }
    }

    workloads
}

fn validate_workload_id(
    workloads: &[FuzzWorkloadOutput],
    workload_id: &str,
) -> homeboy::core::Result<()> {
    if workloads.iter().any(|workload| workload.id == workload_id) {
        return Ok(());
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "workload",
        format!("Unknown fuzz workload '{workload_id}'. Run `homeboy fuzz list` to inspect declared workloads."),
        None,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct FuzzCli {
        #[command(flatten)]
        args: FuzzArgs,
    }

    #[test]
    fn fuzz_run_parses_generic_contract_flags() {
        let cli = FuzzCli::parse_from([
            "fuzz",
            "run",
            "component-a",
            "--workload",
            "parser",
            "--run-id",
            "proof-1",
            "--seed",
            "1234",
            "--max-duration",
            "60s",
            "--",
            "--engine",
            "libfuzzer",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Run(run)) => {
                assert_eq!(run.comp.component.as_deref(), Some("component-a"));
                assert_eq!(run.workload_id.as_deref(), Some("parser"));
                assert_eq!(run.run_id.as_deref(), Some("proof-1"));
                assert_eq!(run.seed.as_deref(), Some("1234"));
                assert_eq!(run.max_duration.as_deref(), Some("60s"));
                assert_eq!(run.args, vec!["--engine", "libfuzzer"]);
            }
            _ => panic!("expected fuzz run command"),
        }
    }

    #[test]
    fn fuzz_output_contract_has_stable_variant_discriminators() {
        let list = serde_json::to_value(FuzzOutput::List(FuzzListOutput {
            command: "fuzz.list".to_string(),
            component: "component-a".to_string(),
            workloads: Vec::new(),
            count: 0,
        }))
        .unwrap();
        assert_eq!(list["variant"], "list");

        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            status: "planned".to_string(),
            workload_id: Some("parser".to_string()),
            run_id: None,
            seed: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: Vec::new(),
            },
        }))
        .unwrap();
        assert_eq!(run["variant"], "run");
    }
}
