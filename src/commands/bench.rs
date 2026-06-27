use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;

use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::bench as extension_bench;
use homeboy::core::extension::bench::{
    aggregate_comparison_with_axes, BenchCommandOutput, BenchComparisonOutput,
    BenchComparisonSummaryOutput, BenchListWorkflowArgs, BenchListWorkflowResult, RigBenchEntry,
    DEFAULT_REGRESSION_THRESHOLD_PERCENT,
};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::rig::{self, RigSpec};

use super::utils::args::{
    filter_passthrough_args, BaselineArgs, ExtensionOverrideArgs, PassthroughCommand,
    PositionalComponentArgs, SettingArgs,
};
use super::{CmdResult, GlobalArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, CommandPortabilityContract,
    LabCommandContract, BENCH_LAB_LABEL,
};

mod default_baseline;
mod fanout;
mod matrix;
mod observation;
mod settings_matrix;

use default_baseline::{
    add_default_baseline_failure_hint, apply_default_baseline_failure_context,
    default_baseline_notice, maybe_expand_default_baseline,
};

#[derive(Args)]
pub struct BenchArgs {
    #[command(subcommand)]
    command: Option<BenchCommand>,

    /// Print the full JSON output instead of the compact human summary.
    /// The compact summary is the default for terminals; the full
    /// structured payload is always written to `--output <file>` and is
    /// printed to stdout with this flag. No data differs between the two —
    /// only the default presentation is compact.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    run: BenchRunArgs,
}

impl BenchArgs {
    /// Whether the caller asked for the full JSON payload on stdout
    /// (suppressing the compact human summary presentation).
    pub fn wants_full_json(&self) -> bool {
        self.json
    }

    /// Whether this invocation is a bench *run* (the variants that emit the
    /// large `BenchCommandOutput`/comparison envelopes the compact summary
    /// targets). Subcommands like `list` keep their existing output.
    pub fn is_run_invocation(&self) -> bool {
        matches!(self.command, None | Some(BenchCommand::Matrix(_)))
    }

    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(CommandJsonFamily::Quality, output_file_mode)
    }

    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        self.is_lab_offload_command().then(|| {
            LabCommandContract::portable(
                BENCH_LAB_LABEL,
                self.lab_offload_writes_local_state()
                    .then_some("--baseline/--ratchet"),
                true,
                &[],
            )
        })
    }

    pub(crate) fn portability_contract(&self) -> CommandPortabilityContract {
        CommandPortabilityContract::lab_optional(self.lab_contract())
    }

    pub fn is_lab_offload_command(&self) -> bool {
        matches!(self.command, None | Some(BenchCommand::Matrix(_)))
    }

    pub fn lab_offload_writes_local_state(&self) -> bool {
        self.run_args_for_lab_offload()
            .is_some_and(|run| run.baseline_args.baseline || run.baseline_args.ratchet)
    }

    pub fn extension_override_ids(&self) -> &[String] {
        self.run_args_for_lab_offload()
            .map(|run| run.extension_override.extensions.as_slice())
            .unwrap_or(&[])
    }

    fn run_args_for_lab_offload(&self) -> Option<&BenchRunArgs> {
        match &self.command {
            None => Some(&self.run),
            Some(BenchCommand::Matrix(args)) => Some(args.run_args()),
            Some(BenchCommand::List(_)) => None,
        }
    }
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum BenchCommand {
    /// Run a local settings matrix and aggregate child bench runs
    Matrix(settings_matrix::BenchMatrixArgs),
    /// List declared benchmark scenarios without executing them
    List(BenchListArgs),
}

#[derive(Args)]
struct BenchListArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    /// Discover scenarios using a rig's component path, extension config,
    /// and rig-declared bench workloads.
    #[arg(long, value_name = "RIG_ID", value_delimiter = ',')]
    rig: Vec<String>,

    /// Only list matching benchmark scenario ids. Repeat to select multiple.
    #[arg(long = "scenario", value_name = "SCENARIO_ID")]
    scenario_ids: Vec<String>,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Additional arguments to pass to the bench runner (must follow --)
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args, Clone)]
pub struct BenchRunArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    /// Iterations per scenario (default 10). Forwarded to the runner via
    /// HOMEBOY_BENCH_ITERATIONS. Individual extensions may clamp.
    #[arg(long, default_value_t = 10)]
    iterations: u64,

    /// Warmup iterations to run before measured iterations. Forwarded to
    /// the runner via HOMEBOY_BENCH_WARMUP_ITERATIONS. When omitted,
    /// rig bench.warmup_iterations may provide the value; otherwise the
    /// runner keeps its own default.
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    warmup: Option<u64>,

    /// Number of repetitions (independent substrate spawns). Default 1
    /// preserves today's exact behaviour. This is a numeric COUNT, not a
    /// proof label — use --run-id to tag a run with a stable identifier.
    /// When > 1, the bench dispatcher is invoked N times in sequence and
    /// per-scenario metrics carry both the cross-run p50 (top-level,
    /// unchanged shape) and a runs array with each run's raw metrics, plus
    /// a runs_summary object with n/min/max/mean/stdev/cv_pct/p50/p95.
    #[arg(long, default_value_t = 1, value_name = "COUNT", value_parser = crate::commands::parse_runs_count)]
    runs: u64,

    /// Caller-supplied stable proof label for this run. Forwarded to
    /// component bench scripts via HOMEBOY_BENCH_RUN_ID so a run can be
    /// correlated across systems (CI logs, dashboards, proof archives).
    /// This is NOT a repetition count — use --runs for that. Components
    /// whose bench runner does not consume HOMEBOY_BENCH_RUN_ID simply
    /// ignore it; homeboy emits a notice rather than a hard error.
    #[arg(long = "run-id", value_name = "ID")]
    run_id: Option<String>,

    /// Directory shared across bench runner instances.
    #[arg(long, value_name = "DIR")]
    shared_state: Option<PathBuf>,

    /// Number of concurrent bench runner instances.
    /// When `--matrix` is used, this controls scheduler task concurrency.
    #[arg(long, default_value_t = 1)]
    concurrency: u32,

    /// Matrix axis in NAME=value,value form. Repeat for multiple axes.
    #[arg(long = "matrix", value_name = "NAME=VALUE[,VALUE...]")]
    matrix: Vec<String>,

    /// Generic agent-task executor backend/runner pool for matrix fan-out.
    #[arg(long = "runner-pool", value_name = "BACKEND")]
    runner_pool: Option<String>,

    /// Cap the number of matrix cells accepted by the scheduler.
    #[arg(long = "max-tasks", value_name = "N")]
    matrix_max_tasks: Option<usize>,

    /// Cap the scheduler queue depth for matrix cells.
    #[arg(long = "max-queue-depth", value_name = "N")]
    matrix_max_queue_depth: Option<usize>,

    /// Artifact name expected from each matrix cell.
    #[arg(long = "expect-artifact", value_name = "NAME")]
    expected_artifact: Vec<String>,

    #[command(flatten)]
    baseline_args: BaselineArgs,

    /// p95 regression tolerance as a percentage. A scenario regresses when
    /// its current p95_ms exceeds baseline.p95_ms * (1 + threshold/100).
    #[arg(long, value_name = "PERCENT", default_value_t = DEFAULT_REGRESSION_THRESHOLD_PERCENT)]
    regression_threshold: f64,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Additional arguments to pass to the bench runner (must follow --)
    #[arg(last = true)]
    args: Vec<String>,

    /// Print compact machine-readable summary (for CI wrappers)
    #[arg(long)]
    json_summary: bool,

    /// Write machine-readable long-loop heartbeat/status JSON to this path.
    /// The file is updated when the observation starts and again when it
    /// finishes or errors.
    #[arg(long, value_name = "PATH")]
    pub(crate) status_file: Option<PathBuf>,

    /// Include a combined comparison report artifact. Currently supports
    /// `side-by-side` for multi-rig bench comparisons.
    #[arg(long = "report", value_enum)]
    report: Vec<BenchReportFormat>,

    /// Run bench against one or more homeboy rigs.
    ///
    /// **Single rig** (`--rig <id>`): pins the rig, runs `rig check`
    /// (aborting on failure), captures component states (git SHA +
    /// branch) into the bench output, and stores the baseline under a
    /// rig-scoped key so rig-pinned and unpinned baselines don't
    /// collide.
    ///
    /// **Multiple rigs** (`--rig <a>,<b>[,<c>...]`): runs the same
    /// component + workload + iteration count against each rig in
    /// sequence by default and emits a `BenchComparisonOutput` envelope with
    /// per-rig results plus a `diff` table of per-metric percent deltas
    /// vs the first rig (the reference). Cross-rig runs are
    /// **comparison-only**: `--baseline` and `--ratchet` are rejected,
    /// because writing one baseline per rig from a comparison
    /// invocation would silently bless one rig over the others. To
    /// ratchet a single rig, run `--rig <id> --baseline` on its own.
    ///
    /// If the rig spec declares `bench.default_component`, the
    /// positional component argument is optional — the rig's default
    /// fills in. With multiple rigs, every rig must agree on the
    /// default (or the positional component must be provided).
    #[arg(long, value_name = "RIG_ID[,RIG_ID...]", value_delimiter = ',')]
    rig: Vec<String>,

    /// Order to use when running a multi-rig comparison. `input` preserves
    /// the --rig list order and keeps the first rig as the comparison
    /// reference. `reverse` flips the order so users can repeat the same
    /// comparison with the opposite cold/warm position when rigs share
    /// external daemon or cache state.
    #[arg(long, value_enum, default_value_t = BenchRigOrder::Input)]
    rig_order: BenchRigOrder,

    /// Number of rigs to run concurrently during a multi-rig comparison.
    /// Default 1 preserves stable sequential CI behavior. Values greater
    /// than 1 opt into bounded parallel rig execution.
    #[arg(long, default_value_t = 1)]
    rig_concurrency: u32,

    /// Only run matching benchmark scenario ids. Repeat to select multiple.
    #[arg(
        long = "scenario",
        value_name = "SCENARIO_ID",
        conflicts_with = "profile"
    )]
    scenario_ids: Vec<String>,

    /// Run the named rig-defined bench profile.
    #[arg(long, value_name = "PROFILE")]
    profile: Option<String>,

    /// Run using env and passthrough args from a single extension-declared CI bench profile.
    #[arg(long, value_name = "ID", conflicts_with = "profile")]
    ci_profile: Option<String>,

    /// Skip auto-upgrading single-rig runs into a comparison even when
    /// the rig spec declares `bench.default_baseline_rig`. Use with
    /// `--baseline` / `--ratchet` against a rig that normally
    /// auto-pairs, or to bench the candidate alone.
    #[arg(long)]
    ignore_default_baseline: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BenchRigOrder {
    Input,
    Reverse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum BenchReportFormat {
    SideBySide,
}

/// Warn — before a (potentially long) bench run starts — when a
/// `--setting` / `--setting-json` key is not declared by the resolved
/// extension's manifest `settings` block.
///
/// Silently accepting a key the component bench script never consumes
/// (e.g. `bench_env` typo vs the correct `workflow_bench_env`) wastes
/// long proof runs: the operator believes they configured the run, but
/// the value was dropped on the floor. A warning is the safe default —
/// it surfaces the typo without failing runs whose extension legitimately
/// declares no settings (where validation can't be performed).
fn warn_unknown_setting_keys(
    ctx: &execution_context::ExecutionContext,
    setting_args: &SettingArgs,
) {
    if let Some(message) = unknown_setting_keys_warning(ctx, setting_args) {
        eprintln!("{message}");
    }
}

/// Build the unknown-setting-key warning message, or `None` when every
/// provided override is declared (or the extension declares no settings,
/// in which case validation is skipped). Split out from
/// [`warn_unknown_setting_keys`] so the message content is unit-testable
/// without capturing stderr.
fn unknown_setting_keys_warning(
    ctx: &execution_context::ExecutionContext,
    setting_args: &SettingArgs,
) -> Option<String> {
    let unknown = ctx.unknown_setting_overrides(
        setting_args.setting.iter().map(|(k, _)| k.as_str()),
        setting_args.setting_json.iter().map(|(k, _)| k.as_str()),
    );
    if unknown.is_empty() {
        return None;
    }

    let mut accepted = ctx.accepted_setting_keys.clone();
    accepted.sort();
    let extension = ctx.extension_id.as_deref().unwrap_or("<unknown>");
    Some(format!(
        "warning: bench ignored {} unknown setting key(s) not declared by extension '{}': {}. \
         Accepted settings: {}. Check for a typo before relying on this run.",
        unknown.len(),
        extension,
        unknown.join(", "),
        if accepted.is_empty() {
            "<none declared>".to_string()
        } else {
            accepted.join(", ")
        },
    ))
}

/// Filter out homeboy-owned flags from trailing args before passing to
/// extension scripts.
///
/// Same pattern as `test.rs::filter_homeboy_flags` — clap's
/// `trailing_var_arg` captures everything after the positional component,
/// including flags that also got parsed into named fields. Without
/// filtering, homeboy-owned flags leak into the extension runner script.
fn filter_homeboy_flags(args: &[String]) -> Vec<String> {
    filter_passthrough_args(PassthroughCommand::Bench, args)
}

/// Tagged output envelope for `homeboy bench`.
#[derive(Serialize)]
#[serde(tag = "variant", content = "payload", rename_all = "snake_case")]
pub enum BenchOutput {
    Single(BenchCommandOutput),
    Comparison(BenchComparisonOutput),
    ComparisonSummary(BenchComparisonSummaryOutput),
    MatrixFanout(fanout::BenchMatrixFanoutOutput),
    SettingsMatrix(settings_matrix::BenchSettingsMatrixOutput),
    List(BenchListWorkflowResult),
}

pub fn run(mut args: BenchArgs, _global: &GlobalArgs) -> CmdResult<BenchOutput> {
    if let Some(command) = &args.command {
        return match command {
            BenchCommand::Matrix(matrix_args) => {
                let output = settings_matrix::run_settings_matrix(matrix_args)?;
                let exit = if output.summary.passed { 0 } else { 1 };
                Ok((BenchOutput::SettingsMatrix(output), exit))
            }
            BenchCommand::List(list_args) => run_list(list_args),
        };
    }

    if !args.run.matrix.is_empty() {
        let output = fanout::run_matrix_fanout(&args.run)?;
        let exit = if output.matrix.passed { 0 } else { 1 };
        return Ok((BenchOutput::MatrixFanout(output), exit));
    }

    // No --rig: single component run. No rig pinning, no rig
    // snapshot, baseline key untouched.
    if args.run.rig.is_empty() {
        validate_report_selection_for_single_run(&args.run)?;
        let passthrough_args = filter_homeboy_flags(&args.run.args);
        let (output, exit) = matrix::run_single(&args.run, &passthrough_args, None)?;
        return Ok((BenchOutput::Single(output), exit));
    }

    // Single --rig <candidate> + spec declares default_baseline_rig +
    // user has not opted out → rewrite args.rig to the canonical
    // [baseline, candidate] comparison shape and tail-call into the
    // multi-rig branch below. Single source of truth for the
    // comparison codepath, no parallel envelope or runner.
    let mut default_baseline_expansion = None;
    if let Some(expansion) = maybe_expand_default_baseline(&args.run)? {
        args.run.rig = expansion.rig_ids.clone();
        let execution_order = ordered_rig_ids(&args.run);
        let metadata = expansion.metadata(execution_order);
        eprintln!("{}", default_baseline_notice(&metadata));
        default_baseline_expansion = Some(metadata);
    }

    let run_args = &args.run;
    let passthrough_args = filter_homeboy_flags(&run_args.args);

    // --rig with one value: single rig-pinned run. A rig that declares
    // bench.components fans out across those components while preserving
    // one rig-state snapshot. Rigs with only default_component keep the
    // one-component payload shape.
    if run_args.rig.len() == 1 {
        validate_report_selection_for_single_run(run_args)?;
        let rig_id = run_args.rig[0].clone();
        let (output, exit) = matrix::run_single_rig(run_args, &passthrough_args, rig_id)?;
        return Ok((BenchOutput::Single(output), exit));
    }

    // --rig with two or more values: cross-rig comparison. Run each rig
    // in sequence by default, or in bounded parallel batches when the user
    // explicitly opts in with --rig-concurrency.
    if run_args.baseline_args.baseline {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--baseline",
            "Cannot --baseline a cross-rig run; baselines are per-rig. \
             Run `homeboy bench --rig <id> --baseline` once per rig you \
             want to ratchet.",
            None,
            None,
        ));
    }
    if run_args.baseline_args.ratchet {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--ratchet",
            "Cannot --ratchet a cross-rig run; baselines are per-rig. \
             Run `homeboy bench --rig <id> --ratchet` once per rig.",
            None,
            None,
        ));
    }
    if let Some(profile) = &run_args.profile {
        matrix::validate_profile_available_for_rigs(&run_args.rig, profile)?;
    }

    let ordered_rigs = ordered_rig_ids(run_args);
    let rig_outputs = match run_cross_rig_benches(run_args, &passthrough_args, ordered_rigs) {
        Ok(outputs) => outputs,
        Err(error) => {
            return Err(add_default_baseline_failure_hint(
                error,
                default_baseline_expansion.as_ref(),
            ));
        }
    };

    let mut entries = Vec::with_capacity(rig_outputs.len());
    let mut effective_component_label: Option<String> = None;
    let mut axes_by_rig = BTreeMap::new();

    for rig_output in rig_outputs {
        if let Some(axes) = rig_output.axes {
            axes_by_rig.insert(rig_output.rig_id.clone(), axes);
        }
        let single_output = rig_output.output;
        if effective_component_label.is_none() {
            effective_component_label = Some(single_output.component.clone());
        }
        entries.push(RigBenchEntry {
            rig_id: rig_output.rig_id,
            passed: single_output.passed,
            status: single_output.status,
            exit_code: single_output.exit_code,
            artifacts: single_output.artifacts,
            results: single_output.results,
            rig_state: single_output.rig_state,
            failure: single_output.failure,
            diagnostics: single_output.diagnostics,
        });
    }

    let component = effective_component_label
        .or_else(|| run_args.comp.id().map(|s| s.to_string()))
        .unwrap_or_else(|| "<unknown>".to_string());

    let (mut output, exit) =
        aggregate_comparison_with_axes(component, run_args.iterations, entries, &axes_by_rig);
    if let Some(metadata) = default_baseline_expansion {
        apply_default_baseline_failure_context(&mut output, &metadata);
        output.default_baseline_expansion = Some(metadata);
    }
    if run_args.json_summary {
        return Ok((BenchOutput::ComparisonSummary(output.into()), exit));
    }
    Ok((BenchOutput::Comparison(output), exit))
}

struct CrossRigBenchOutput {
    rig_id: String,
    axes: Option<BTreeMap<String, String>>,
    output: BenchCommandOutput,
}

fn run_cross_rig_benches(
    run_args: &BenchRunArgs,
    passthrough_args: &[String],
    ordered_rigs: Vec<String>,
) -> homeboy::core::Result<Vec<CrossRigBenchOutput>> {
    if run_args.rig_concurrency <= 1 || ordered_rigs.len() <= 1 {
        return ordered_rigs
            .into_iter()
            .map(|rig_id| run_cross_rig_bench(run_args, passthrough_args, rig_id))
            .collect();
    }

    let concurrency = run_args.rig_concurrency as usize;
    let mut outputs = Vec::with_capacity(ordered_rigs.len());

    for chunk in ordered_rigs.chunks(concurrency) {
        let mut chunk_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(chunk.len());
            for rig_id in chunk.iter().cloned() {
                handles.push(
                    scope.spawn(move || run_cross_rig_bench(run_args, passthrough_args, rig_id)),
                );
            }

            handles
                .into_iter()
                .map(|handle| match handle.join() {
                    Ok(result) => result,
                    Err(_) => Err(homeboy::core::Error::internal_unexpected(
                        "bench rig worker panicked during parallel comparison",
                    )),
                })
                .collect::<homeboy::core::Result<Vec<_>>>()
        })?;
        outputs.append(&mut chunk_outputs);
    }

    Ok(outputs)
}

fn run_cross_rig_bench(
    run_args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_id: String,
) -> homeboy::core::Result<CrossRigBenchOutput> {
    let axes = rig_axes(&rig_id)?;
    let (output, _exit) = matrix::run_single(run_args, passthrough_args, Some(rig_id.clone()))?;
    Ok(CrossRigBenchOutput {
        rig_id,
        axes,
        output,
    })
}

fn rig_axes(rig_id: &str) -> homeboy::core::Result<Option<BTreeMap<String, String>>> {
    let spec = rig::load(rig_id)?;
    let Some(bench) = spec.bench else {
        return Ok(None);
    };
    if bench.axes.is_empty() {
        Ok(None)
    } else {
        Ok(Some(bench.axes))
    }
}

fn ordered_rig_ids(args: &BenchRunArgs) -> Vec<String> {
    let mut rig_ids = args.rig.clone();
    if args.rig_order == BenchRigOrder::Reverse {
        rig_ids.reverse();
    }
    rig_ids
}

fn validate_report_selection_for_single_run(args: &BenchRunArgs) -> homeboy::core::Result<()> {
    if args.report.is_empty() {
        return Ok(());
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "--report",
        "Bench reports are only available for multi-rig comparisons. Pass two or more --rig values, for example: --rig baseline,candidate --report side-by-side.",
        None,
        None,
    ))
}

fn run_list(args: &BenchListArgs) -> CmdResult<BenchOutput> {
    let passthrough_args = filter_homeboy_flags(&args.args);
    let rig_context = load_list_rig(args)?;
    if let Some(context) = rig_context.as_ref() {
        report_list_rig_source(context);
    }
    let rig_spec = rig_context.as_ref().map(|context| &context.spec);
    let effective_id = resolve_list_component_id(args, rig_spec)?;
    let path_override = args.comp.path.clone().or_else(|| {
        rig_spec
            .as_ref()
            .and_then(|spec| matrix::rig_component_path(spec, &effective_id))
    });
    let component_override = rig_spec
        .as_ref()
        .and_then(|spec| matrix::rig_component_for_bench(spec, &effective_id));

    let mut resolve_options = ResolveOptions::with_capability_and_json(
        &effective_id,
        path_override.clone(),
        ExtensionCapability::Bench,
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
    );
    resolve_options.extension_overrides =
        effective_extension_overrides(&args.extension_override.extensions, rig_spec.as_deref());

    let ctx = execution_context::resolve_with_component(&resolve_options, component_override)?;
    warn_unknown_setting_keys(&ctx, &args.setting_args);
    let extra_workloads = rig_spec
        .as_ref()
        .and_then(|spec| {
            ctx.extension_id.as_deref().map(|id| {
                rig::workloads_for_extension(
                    spec,
                    rig::RigWorkloadKind::Bench,
                    rig_context
                        .as_ref()
                        .and_then(|context| context.package_root.as_deref()),
                    id,
                )
            })
        })
        .unwrap_or_default();

    let run_dir = RunDir::create()?;
    let resource_run = homeboy::core::engine::resource::ResourceSummaryRun::start(Some(format!(
        "bench list {}",
        effective_id
    )));
    let output = extension_bench::run_bench_list_workflow(
        &ctx.component,
        BenchListWorkflowArgs {
            component_label: effective_id,
            component_id: ctx.component_id.clone(),
            path_override,
            settings: ctx.resolved_settings().string_overrides(),
            settings_json: ctx.resolved_settings().json_overrides(),
            passthrough_args,
            scenario_ids: args.scenario_ids.clone(),
            extra_workloads,
            env_provider_extensions: rig_spec
                .as_ref()
                .and_then(|spec| {
                    ctx.extension_id.as_deref().map(|id| {
                        rig::env_provider_extensions_for_extension_workloads(
                            spec,
                            rig::RigWorkloadKind::Bench,
                            id,
                        )
                    })
                })
                .unwrap_or_default(),
            rig_package: rig_context
                .as_ref()
                .and_then(|context| rig::package_evidence(&context.spec.id)),
        },
        &run_dir,
    );
    resource_run.write_to_run_dir(&run_dir)?;
    let output = output?;

    Ok((BenchOutput::List(output), 0))
}

fn report_list_rig_source(context: &ListRigContext) {
    if let Some(evidence) = rig::package_evidence(&context.spec.id) {
        eprintln!(
            "bench rig source: rig={} package_root={} freshness={:?}",
            evidence.rig_id, evidence.package_root, evidence.freshness
        );
    }
}

fn effective_extension_overrides(
    explicit_overrides: &[String],
    rig_spec: Option<&rig::RigSpec>,
) -> Vec<String> {
    if !explicit_overrides.is_empty() {
        return explicit_overrides.to_vec();
    }

    let Some(spec) = rig_spec else {
        return Vec::new();
    };

    let workload_extension_ids =
        rig::extension_ids_for_workloads(spec, rig::RigWorkloadKind::Bench);
    match workload_extension_ids.as_slice() {
        [extension_id] => vec![extension_id.clone()],
        _ => Vec::new(),
    }
}

type ListRigContext = rig::RigSourceContext;

fn load_list_rig(args: &BenchListArgs) -> homeboy::core::Result<Option<ListRigContext>> {
    match args.rig.as_slice() {
        [] => Ok(None),
        [rig_id] => Ok(Some(rig::RigSourceContext::load_for_invocation(rig_id)?)),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "--rig",
            "bench list accepts exactly one rig id",
            None,
            None,
        )),
    }
}

fn resolve_list_component_id(
    args: &BenchListArgs,
    rig_spec: Option<&RigSpec>,
) -> homeboy::core::Result<String> {
    if let Some(id) = args.comp.id() {
        return Ok(id.to_string());
    }

    if let Some(spec) = rig_spec {
        if let Some(default) = spec
            .bench
            .as_ref()
            .and_then(|bench| matrix::bench_component_ids(bench).into_iter().next())
        {
            return Ok(default);
        }

        return Err(homeboy::core::Error::validation_invalid_argument(
            "bench.default_component",
            format!(
                "rig '{}' does not declare bench.default_component; pass a component id or add bench.default_component to the rig spec",
                spec.id
            ),
            None,
            None,
        ));
    }

    args.comp.resolve_id()
}

#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod tests;
