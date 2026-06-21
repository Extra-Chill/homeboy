use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use homeboy::core::component::{Component, ScopedExtensionConfig};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::{self, ExtensionCapability, ExtensionRunner};
use homeboy::core::fuzz::{parse_fuzz_results_file, FuzzCampaign};
use homeboy::core::rig::{self, RigSpec};

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

    /// Discover workloads using a rig's component path, extension config, and
    /// rig-declared fuzz workloads.
    #[arg(long, value_name = "RIG_ID")]
    rig: Option<String>,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    setting_args: SettingArgs,
}

#[derive(Args, Clone)]
pub struct FuzzRunArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Run against a rig's component path, extension config, and rig-declared
    /// fuzz workloads.
    #[arg(long, value_name = "RIG_ID")]
    rig: Option<String>,

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
    pub rig_id: Option<String>,
    pub workloads: Vec<FuzzWorkloadOutput>,
    pub count: usize,
    pub run_hint: String,
}

#[derive(Serialize)]
pub struct FuzzRunOutput {
    pub kind: String,
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub status: String,
    pub workload_id: Option<String>,
    pub workload_path: Option<String>,
    pub run_id: Option<String>,
    pub seed: Option<String>,
    pub max_duration: Option<String>,
    pub passthrough_args: Vec<String>,
    pub execution: Option<FuzzExecutionOutput>,
    pub results: Option<FuzzCampaign>,
    pub runner_contract: FuzzRunnerContract,
    pub evidence_followups: Vec<String>,
}

#[derive(Serialize)]
pub struct FuzzExecutionOutput {
    pub kind: String,
    pub extension_id: String,
    pub exit_code: i32,
    pub success: bool,
    pub run_dir: String,
    pub results_file: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FuzzWorkloadOutput {
    pub id: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub source: String,
    pub manifest_path: Option<String>,
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
        Some(FuzzCommand::Run(run_args)) => {
            let (output, exit) = run_run(run_args)?;
            Ok((FuzzOutput::Run(output), exit))
        }
        None => {
            let (output, exit) = run_run(args.run)?;
            Ok((FuzzOutput::Run(output), exit))
        }
    }
}

fn run_list(args: FuzzListArgs) -> homeboy::core::Result<FuzzListOutput> {
    let rig_context = load_rig(args.rig.as_deref())?;
    let effective_id = resolve_component_id(
        &args.comp,
        rig_context.as_ref().map(|context| &context.spec),
    )?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );

    Ok(FuzzListOutput {
        command: "fuzz.list".to_string(),
        component: ctx.component_id,
        rig_id: rig_context.map(|context| context.spec.id),
        count: workloads.len(),
        workloads,
        run_hint: "Select one workload with `homeboy fuzz run <component> --workload <id>`; offload heavy campaigns with the global `--runner <id>` flag when configured.".to_string(),
    })
}

fn run_run(args: FuzzRunArgs) -> homeboy::core::Result<(FuzzRunOutput, i32)> {
    let rig_context = load_rig(args.rig.as_deref())?;
    let effective_id = resolve_component_id(
        &args.comp,
        rig_context.as_ref().map(|context| &context.spec),
    )?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );
    let selected_workload = select_workload(&workloads, args.workload_id.as_deref())?;
    let run_dir = RunDir::create()?;
    let runner_output = run_fuzz_extension_script(&ctx, &args, selected_workload, &run_dir)?;
    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let results = if results_path.exists() {
        Some(parse_fuzz_results_file(&results_path)?)
    } else {
        None
    };
    let exit_code = runner_output.exit_code;
    let success = runner_output.success;
    let evidence_followups = fuzz_evidence_followups(args.run_id.as_deref());

    Ok((
        FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: ctx.component_id,
            rig_id: rig_context.map(|context| context.spec.id),
            status: if success { "passed" } else { "failed" }.to_string(),
            workload_id: selected_workload
                .map(|workload| workload.id.clone())
                .or(args.workload_id),
            workload_path: selected_workload.and_then(|workload| workload.manifest_path.clone()),
            run_id: args.run_id,
            seed: args.seed,
            max_duration: args.max_duration,
            passthrough_args: args.args,
            execution: Some(FuzzExecutionOutput {
                kind: "fuzz".to_string(),
                extension_id: ctx.extension_id.unwrap_or_default(),
                exit_code,
                success,
                run_dir: run_dir.path().to_string_lossy().to_string(),
                results_file: results_path.to_string_lossy().to_string(),
                stdout: runner_output.stdout,
                stderr: runner_output.stderr,
            }),
            results,
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: vec![
                    "HOMEBOY_FUZZ_RESULTS_FILE",
                    "HOMEBOY_FUZZ_WORKLOAD_ID",
                    "HOMEBOY_FUZZ_WORKLOAD_PATH",
                    "HOMEBOY_FUZZ_RUN_ID",
                    "HOMEBOY_FUZZ_SEED",
                    "HOMEBOY_FUZZ_MAX_DURATION",
                ],
            },
            evidence_followups,
        },
        exit_code,
    ))
}

fn fuzz_evidence_followups(run_id: Option<&str>) -> Vec<String> {
    match run_id.filter(|run_id| !run_id.trim().is_empty()) {
        Some(run_id) => vec![
            format!("homeboy runs show {run_id}"),
            format!("homeboy runs evidence {run_id}"),
            format!("homeboy runs artifacts {run_id}"),
        ],
        None => vec![
            "Use --run-id <stable-id> when the downstream runner records persisted Homeboy evidence.".to_string(),
            "Inspect persisted proof with `homeboy runs show <run-id>` and `homeboy runs evidence <run-id>`.".to_string(),
        ],
    }
}

fn run_fuzz_extension_script(
    ctx: &execution_context::ExecutionContext,
    args: &FuzzRunArgs,
    workload: Option<&FuzzWorkloadOutput>,
    run_dir: &RunDir,
) -> homeboy::core::Result<homeboy::core::extension::RunnerOutput> {
    let execution_context =
        extension::resolve_execution_context(&ctx.component, ExtensionCapability::Fuzz)?;
    let mut runner = ExtensionRunner::for_context(execution_context)
        .component(ctx.component.clone())
        .settings(&args.setting_args.setting)
        .settings_json(&args.setting_args.setting_json)
        .path_override(args.comp.path.clone())
        .with_run_dir(run_dir)
        .script_args(&args.args);

    let env = fuzz_runner_env(args, workload);
    for (key, value) in env {
        runner = runner.env(&key, &value);
    }

    runner.run()
}

fn fuzz_runner_env(
    args: &FuzzRunArgs,
    workload: Option<&FuzzWorkloadOutput>,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(workload) = workload {
        env.push(("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), workload.id.clone()));
        if let Some(path) = workload.manifest_path.as_ref() {
            env.push(("HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(), path.clone()));
        }
    }
    push_opt_env(&mut env, "HOMEBOY_FUZZ_RUN_ID", args.run_id.as_ref());
    push_opt_env(&mut env, "HOMEBOY_FUZZ_SEED", args.seed.as_ref());
    push_opt_env(
        &mut env,
        "HOMEBOY_FUZZ_MAX_DURATION",
        args.max_duration.as_ref(),
    );
    env
}

fn push_opt_env(env: &mut Vec<(String, String)>, key: &str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        env.push((key.to_string(), value.clone()));
    }
}

type FuzzRigContext = rig::RigSourceContext;

fn load_rig(rig_id: Option<&str>) -> homeboy::core::Result<Option<FuzzRigContext>> {
    let Some(rig_id) = rig_id else {
        return Ok(None);
    };
    Ok(Some(rig::RigSourceContext::load(rig_id)?))
}

fn resolve_component_id(
    comp: &PositionalComponentArgs,
    rig_spec: Option<&RigSpec>,
) -> homeboy::core::Result<String> {
    if let Some(id) = comp.id() {
        return Ok(id.to_string());
    }

    if let Some(spec) = rig_spec {
        if let Some(default) = spec
            .fuzz
            .as_ref()
            .and_then(|fuzz| fuzz.default_component.as_deref())
        {
            return Ok(default.to_string());
        }

        return Err(homeboy::core::Error::validation_invalid_argument(
            "fuzz.default_component",
            format!(
                "rig '{}' does not declare fuzz.default_component; pass a component id or add fuzz.default_component to the rig spec",
                spec.id
            ),
            None,
            None,
        ));
    }

    comp.resolve_id()
}

fn resolve_fuzz_context(
    component_id: &str,
    comp: &PositionalComponentArgs,
    settings: &SettingArgs,
    extension_override: &ExtensionOverrideArgs,
    capability: ExtensionCapability,
    rig_context: Option<&FuzzRigContext>,
) -> homeboy::core::Result<execution_context::ExecutionContext> {
    let rig_spec = rig_context.map(|context| &context.spec);
    let path_override = comp
        .path
        .clone()
        .or_else(|| rig_spec.and_then(|spec| rig_component_path(spec, component_id)));
    let component_override = rig_spec.and_then(|spec| rig_component_for_fuzz(spec, component_id));

    let mut resolve_options = ResolveOptions::with_capability_and_json(
        component_id,
        path_override,
        capability,
        settings.setting.clone(),
        settings.setting_json.clone(),
    );
    resolve_options.extension_overrides = extension_override.extensions.clone();

    execution_context::resolve_with_component(&resolve_options, component_override)
}

fn rig_component_path(spec: &RigSpec, component_id: &str) -> Option<String> {
    spec.components
        .get(component_id)
        .map(|component| rig::expand::expand_vars(spec, &component.path))
}

fn rig_component_for_fuzz(spec: &RigSpec, component_id: &str) -> Option<Component> {
    let rig_component = spec.components.get(component_id)?;
    let mut extensions = rig_component.extensions.clone()?;
    expand_rig_extension_settings(spec, &mut extensions);
    let mut component = Component {
        id: component_id.to_string(),
        local_path: rig::expand::expand_vars(spec, &rig_component.path),
        remote_url: rig_component.remote_url.clone(),
        extensions: Some(extensions),
        ..Component::default()
    };
    component.resolve_remote_path();
    Some(component)
}

fn expand_rig_extension_settings(
    spec: &RigSpec,
    extensions: &mut HashMap<String, ScopedExtensionConfig>,
) {
    for extension in extensions.values_mut() {
        for value in extension.settings.values_mut() {
            expand_rig_setting_value(spec, value);
        }
    }
}

fn expand_rig_setting_value(spec: &RigSpec, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(raw) => {
            *raw = rig::expand::expand_vars(spec, raw);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                expand_rig_setting_value(spec, value);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                expand_rig_setting_value(spec, value);
            }
        }
        _ => {}
    }
}

fn fuzz_workloads(
    component: &homeboy::core::component::Component,
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
) -> Vec<FuzzWorkloadOutput> {
    let mut workloads: Vec<FuzzWorkloadOutput> = component
        .script_commands(ExtensionCapability::Fuzz)
        .iter()
        .enumerate()
        .map(|(index, _command)| FuzzWorkloadOutput {
            id: format!("component-script-{}", index + 1),
            label: None,
            description: None,
            source: "component.scripts.fuzz".to_string(),
            manifest_path: None,
        })
        .collect();

    if let (Some(context), Some(extension_id)) = (rig_context, extension_id) {
        workloads.extend(
            rig::workload_path_expansions_for_extension(
                &context.spec,
                rig::RigWorkloadKind::Fuzz,
                context.package_root.as_deref(),
                extension_id,
            )
            .into_iter()
            .map(|expansion| fuzz_workload_from_path(extension_id, &expansion.expanded_path)),
        );
    }

    if let Some(extensions) = component.extensions.as_ref() {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                workloads.extend(manifest.fuzz_workloads().iter().map(|workload| {
                    FuzzWorkloadOutput {
                        id: workload.id.clone(),
                        label: workload.label.clone(),
                        description: workload.description.clone(),
                        source: format!("extension:{extension_id}"),
                        manifest_path: None,
                    }
                }));
            }
        }
    }

    workloads
}

fn fuzz_workload_from_path(extension_id: &str, path: &Path) -> FuzzWorkloadOutput {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("rig-fuzz-workload")
        .to_string();
    FuzzWorkloadOutput {
        id,
        label: path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        description: None,
        source: format!("rig_workloads:{extension_id}:{}", path.to_string_lossy()),
        manifest_path: Some(path.to_string_lossy().to_string()),
    }
}

fn select_workload<'a>(
    workloads: &'a [FuzzWorkloadOutput],
    workload_id: Option<&str>,
) -> homeboy::core::Result<Option<&'a FuzzWorkloadOutput>> {
    if let Some(workload_id) = workload_id {
        return workloads
            .iter()
            .find(|workload| workload.id == workload_id)
            .map(Some)
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "workload",
                    format!("Unknown fuzz workload '{workload_id}'. Run `homeboy fuzz list` to inspect declared workloads."),
                    None,
                    None,
                )
            });
    }

    if workloads.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workload",
            "No fuzz workloads are declared for this component/rig/extension selection",
            None,
            Some(vec![
                "Run `homeboy fuzz list <component> --rig <id>` to inspect the resolved selection.".to_string(),
                "Declare extension fuzz workloads, component scripts.fuzz commands, or rig fuzz_workloads before claiming fuzz coverage.".to_string(),
                "If the command is available in source but not on the Lab runner, run `homeboy lab status --runner <id>` and refresh or upgrade the runner binary.".to_string(),
            ]),
        ));
    }

    let mut path_workloads = workloads
        .iter()
        .filter(|workload| workload.manifest_path.is_some());
    let first = path_workloads.next();
    if first.is_some() && path_workloads.next().is_none() {
        return Ok(first);
    }

    if workloads.len() > 1 {
        let workload_ids = workloads
            .iter()
            .map(|workload| workload.id.clone())
            .collect::<Vec<_>>();
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workload",
            "Multiple fuzz workloads are declared; select one explicitly with --workload <id>",
            None,
            Some(vec![
                format!("Available workload ids: {}", workload_ids.join(", ")),
                "Run `homeboy fuzz list` for labels, descriptions, sources, and manifest paths."
                    .to_string(),
            ]),
        ));
    }

    Ok(None)
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
            "--rig",
            "package-fuzz",
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
                assert_eq!(run.rig.as_deref(), Some("package-fuzz"));
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
            rig_id: None,
            workloads: Vec::new(),
            count: 0,
            run_hint: "hint".to_string(),
        }))
        .unwrap();
        assert_eq!(list["variant"], "list");

        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            rig_id: Some("package-fuzz".to_string()),
            status: "passed".to_string(),
            workload_id: Some("parser".to_string()),
            workload_path: None,
            run_id: None,
            seed: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            execution: None,
            results: None,
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: Vec::new(),
            },
            evidence_followups: Vec::new(),
        }))
        .unwrap();
        assert_eq!(run["variant"], "run");
        assert_eq!(run["kind"], "fuzz");
        assert_eq!(run["rig_id"], "package-fuzz");
    }

    #[test]
    fn fuzz_workloads_include_rig_declared_paths() {
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": "/tmp/package",
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "fuzz": {
                "default_component": "package"
            },
            "fuzz_workloads": {
                "generic": [
                    { "path": "${package.root}/fuzz/checkout-create-order.json" }
                ]
            }
        }))
        .expect("parse rig spec");
        let component = rig_component_for_fuzz(&spec, "package").expect("rig component");
        let context = FuzzRigContext {
            spec,
            package_root: Some(std::path::PathBuf::from("/tmp/homeboy-rigs/package")),
        };

        let workloads = fuzz_workloads(&component, Some(&context), Some("generic"));

        assert!(workloads.iter().any(|workload| {
            workload.id == "checkout-create-order"
                && workload.manifest_path.as_deref()
                    == Some("/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json")
                && workload.source
                    == "rig_workloads:generic:/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json"
        }));
    }

    #[test]
    fn resolve_component_id_uses_fuzz_default_component() {
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "fuzz": {
                "default_component": "package"
            }
        }))
        .expect("parse rig spec");
        let comp = PositionalComponentArgs {
            component: None,
            path: None,
        };

        assert_eq!(
            resolve_component_id(&comp, Some(&spec)).expect("resolve component"),
            "package"
        );
    }

    #[test]
    fn fuzz_runner_env_includes_selected_workload_path_and_generic_contract() {
        let args = FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("component-a".to_string()),
                path: None,
            },
            rig: None,
            extension_override: ExtensionOverrideArgs { extensions: vec![] },
            setting_args: SettingArgs {
                setting: vec![],
                setting_json: vec![],
            },
            workload_id: Some("parser".to_string()),
            run_id: Some("proof-1".to_string()),
            seed: Some("1234".to_string()),
            max_duration: Some("60s".to_string()),
            args: vec![],
        };
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: "rig_workloads:generic:/tmp/fuzz/parser.json".to_string(),
            manifest_path: Some("/tmp/fuzz/parser.json".to_string()),
        };

        let env = fuzz_runner_env(&args, Some(&workload));

        assert!(env
            .iter()
            .all(|(key, _)| key != "HOMEBOY_FUZZ_RESULTS_FILE"));
        assert!(env.contains(&("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), "parser".to_string())));
        assert!(env.contains(&(
            "HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(),
            "/tmp/fuzz/parser.json".to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_RUN_ID".to_string(), "proof-1".to_string())));
        assert!(env.contains(&("HOMEBOY_FUZZ_SEED".to_string(), "1234".to_string())));
        assert!(env.contains(&("HOMEBOY_FUZZ_MAX_DURATION".to_string(), "60s".to_string())));
    }

    #[test]
    fn fuzz_output_contract_includes_results_file_and_parsed_campaign() {
        let results = FuzzCampaign {
            schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
            id: "campaign-1".to_string(),
            title: None,
            safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            workloads: Vec::new(),
            seeds: Vec::new(),
            coverage: Vec::new(),
            findings: Vec::new(),
            artifacts: Vec::new(),
            thresholds: Vec::new(),
            provenance: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };
        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            rig_id: None,
            status: "passed".to_string(),
            workload_id: None,
            workload_path: None,
            run_id: None,
            seed: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            execution: Some(FuzzExecutionOutput {
                kind: "fuzz".to_string(),
                extension_id: "generic".to_string(),
                exit_code: 0,
                success: true,
                run_dir: "/tmp/homeboy-run".to_string(),
                results_file: "/tmp/homeboy-run/fuzz-results.json".to_string(),
                stdout: String::new(),
                stderr: String::new(),
            }),
            results: Some(results),
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: vec!["HOMEBOY_FUZZ_RESULTS_FILE"],
            },
            evidence_followups: Vec::new(),
        }))
        .unwrap();

        assert_eq!(
            run["execution"]["results_file"],
            "/tmp/homeboy-run/fuzz-results.json"
        );
        assert_eq!(
            run["results"]["schema"],
            homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA
        );
        assert_eq!(run["results"]["id"], "campaign-1");
        assert_eq!(
            run["runner_contract"]["env"][0],
            "HOMEBOY_FUZZ_RESULTS_FILE"
        );
    }

    #[test]
    fn select_workload_requires_explicit_id_for_ambiguous_fuzz_workloads() {
        let workloads = vec![
            FuzzWorkloadOutput {
                id: "parser".to_string(),
                label: None,
                description: None,
                source: "extension:generic".to_string(),
                manifest_path: None,
            },
            FuzzWorkloadOutput {
                id: "serializer".to_string(),
                label: None,
                description: None,
                source: "extension:generic".to_string(),
                manifest_path: None,
            },
        ];

        let err = select_workload(&workloads, None).expect_err("ambiguous workload");

        assert!(err.message.contains("Multiple fuzz workloads"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("parser, serializer")));
    }

    #[test]
    fn select_workload_rejects_empty_fuzz_selection() {
        let err = select_workload(&[], None).expect_err("empty workload selection");

        assert!(err.message.contains("No fuzz workloads"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("fuzz list")));
    }

    #[test]
    fn fuzz_command_tests_keep_core_fixtures_product_neutral() {
        let source = include_str!("fuzz.rs").to_ascii_lowercase();
        let forbidden = ["word", "press"].concat();
        assert!(!source.contains(&forbidden));
    }
}
