use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::{Path, PathBuf};

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::engine::run_dir::RunDir;
use homeboy::extension::bench as extension_bench;
use homeboy::extension::bench::{
    aggregate_comparison, BenchCommandOutput, BenchComparisonOutput, BenchListWorkflowArgs,
    BenchListWorkflowResult, RigBenchEntry, DEFAULT_REGRESSION_THRESHOLD_PERCENT,
};
use homeboy::extension::ExtensionCapability;
use homeboy::rig::{self, RigSpec};

use super::utils::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

mod matrix;

#[derive(Args)]
pub struct BenchArgs {
    #[command(subcommand)]
    command: Option<BenchCommand>,

    #[command(flatten)]
    run: BenchRunArgs,
}

#[derive(Subcommand)]
enum BenchCommand {
    /// List declared benchmark scenarios without executing them
    List(BenchListArgs),
}

#[derive(Args)]
struct BenchListArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

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

#[derive(Args)]
pub struct BenchRunArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

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

    /// Number of independent substrate spawns. Default 1 preserves today's
    /// exact behaviour. When > 1, the bench dispatcher is invoked N times in
    /// sequence and per-scenario metrics carry both the cross-run p50
    /// (top-level, unchanged shape) and a runs array with each run's raw
    /// metrics, plus a runs_summary object with n/min/max/mean/stdev/cv_pct/p50/p95.
    #[arg(long, default_value_t = 1)]
    runs: u64,

    /// Directory shared across bench runner instances.
    #[arg(long, value_name = "DIR")]
    shared_state: Option<PathBuf>,

    /// Number of concurrent bench runner instances.
    #[arg(long, default_value_t = 1)]
    concurrency: u32,

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

    #[command(flatten)]
    _json: HiddenJsonArgs,

    /// Print compact machine-readable summary (for CI wrappers)
    #[arg(long)]
    json_summary: bool,

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
    /// sequence and emits a `BenchComparisonOutput` envelope with
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

/// Filter out homeboy-owned flags from trailing args before passing to
/// extension scripts.
///
/// Same pattern as `test.rs::filter_homeboy_flags` — clap's
/// `trailing_var_arg` captures everything after the positional component,
/// including flags that also got parsed into named fields. Without
/// filtering, homeboy-owned flags leak into the extension runner script.
fn filter_homeboy_flags(args: &[String]) -> Vec<String> {
    const HOMEBOY_FLAGS: &[&str] = &[
        "--baseline",
        "--ignore-baseline",
        "--ignore-default-baseline",
        "--ratchet",
        "--json-summary",
        "--json",
    ];

    const HOMEBOY_VALUE_FLAGS: &[&str] = &[
        "--iterations",
        "--warmup",
        "--runs",
        "--shared-state",
        "--concurrency",
        "--regression-threshold",
        "--scenario",
        "--profile",
        "--rig-order",
        "--rig",
        "--setting",
        "--path",
    ];

    let mut filtered = Vec::new();
    let mut skip_next = false;

    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }

        if HOMEBOY_FLAGS.contains(&arg.as_str()) {
            continue;
        }

        let is_value_flag = HOMEBOY_VALUE_FLAGS.iter().any(|f| {
            if arg.starts_with(&format!("{}=", f)) {
                return true;
            }
            if arg == *f {
                skip_next = true;
                return true;
            }
            false
        });

        if is_value_flag {
            continue;
        }

        filtered.push(arg.clone());
    }

    filtered
}

/// Output envelope for `homeboy bench`.
///
/// Two shapes:
/// - `Single` — bare `bench`, `bench <component>`, or `bench --rig <id>`.
///   Indistinguishable from the pre-cross-rig output for backward
///   compatibility (`#[serde(untagged)]`, no wrapper key).
/// - `Comparison` — `bench --rig <a>,<b>[,...]`. Has a top-level
///   `comparison: "cross_rig"` discriminator field that consumers can
///   check.
#[derive(Serialize)]
#[serde(untagged)]
pub enum BenchOutput {
    Single(BenchCommandOutput),
    Comparison(BenchComparisonOutput),
    List(BenchListWorkflowResult),
}

pub fn run(args: BenchArgs, _global: &GlobalArgs) -> CmdResult<BenchOutput> {
    if let Some(command) = &args.command {
        return match command {
            BenchCommand::List(list_args) => run_list(list_args),
        };
    }

    let run_args = &args.run;
    let passthrough_args = filter_homeboy_flags(&run_args.args);

    // No --rig: legacy single bare run. No rig pinning, no rig
    // snapshot, baseline key untouched. Identical to before this PR.
    if run_args.rig.is_empty() {
        let (output, exit) = matrix::run_single(run_args, &passthrough_args, None)?;
        return Ok((BenchOutput::Single(output), exit));
    }

    // Single --rig <candidate> + spec declares default_baseline_rig +
    // user has not opted out → rewrite args.rig to the canonical
    // [baseline, candidate] comparison shape and tail-call into the
    // multi-rig branch below. Single source of truth for the
    // comparison codepath, no parallel envelope or runner.
    //
    // The recursive call cannot loop: the second invocation has
    // args.rig.len() == 2 and skips this expansion entirely.
    if let Some(expanded) = maybe_expand_default_baseline(run_args)? {
        let mut expanded_args = args;
        expanded_args.run.rig = expanded;
        return run(expanded_args, _global);
    }

    // --rig with one value: single rig-pinned run. A rig that declares
    // bench.components fans out across those components while preserving
    // one rig-state snapshot. Rigs with only default_component keep the
    // legacy one-component shape.
    if run_args.rig.len() == 1 {
        let rig_id = run_args.rig[0].clone();
        let (output, exit) = matrix::run_single_rig(run_args, &passthrough_args, rig_id)?;
        return Ok((BenchOutput::Single(output), exit));
    }

    // --rig with two or more values: cross-rig comparison. Run each rig
    // in sequence, collect per-rig outputs, aggregate into a
    // BenchComparisonOutput.
    if run_args.baseline_args.baseline {
        return Err(homeboy::Error::validation_invalid_argument(
            "--baseline",
            "Cannot --baseline a cross-rig run; baselines are per-rig. \
             Run `homeboy bench --rig <id> --baseline` once per rig you \
             want to ratchet.",
            None,
            None,
        ));
    }
    if run_args.baseline_args.ratchet {
        return Err(homeboy::Error::validation_invalid_argument(
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

    let mut entries = Vec::with_capacity(run_args.rig.len());
    let mut effective_component_label: Option<String> = None;

    let ordered_rigs = ordered_rig_ids(run_args);

    for rig_id in ordered_rigs {
        let (single_output, _exit) =
            matrix::run_single(run_args, &passthrough_args, Some(rig_id.clone()))?;
        if effective_component_label.is_none() {
            effective_component_label = Some(single_output.component.clone());
        }
        entries.push(RigBenchEntry {
            rig_id: rig_id.clone(),
            passed: single_output.passed,
            status: single_output.status,
            exit_code: single_output.exit_code,
            artifacts: single_output.artifacts,
            results: single_output.results,
            rig_state: single_output.rig_state,
            failure: single_output.failure,
        });
    }

    let component = effective_component_label
        .or_else(|| run_args.comp.id().map(|s| s.to_string()))
        .unwrap_or_else(|| "<unknown>".to_string());

    let (output, exit) = aggregate_comparison(component, run_args.iterations, entries);
    Ok((BenchOutput::Comparison(output), exit))
}

fn ordered_rig_ids(args: &BenchRunArgs) -> Vec<String> {
    let mut rig_ids = args.rig.clone();
    if args.rig_order == BenchRigOrder::Reverse {
        rig_ids.reverse();
    }
    rig_ids
}

fn run_list(args: &BenchListArgs) -> CmdResult<BenchOutput> {
    let passthrough_args = filter_homeboy_flags(&args.args);
    let rig_context = load_list_rig(args)?;
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

    let ctx = execution_context::resolve_with_component(
        &ResolveOptions::with_capability_and_json(
            &effective_id,
            path_override.clone(),
            ExtensionCapability::Bench,
            args.setting_args.setting.clone(),
            args.setting_args.setting_json.clone(),
        ),
        component_override,
    )?;

    let extra_workloads = rig_spec
        .as_ref()
        .and_then(|spec| {
            ctx.extension_id.as_deref().map(|id| {
                bench_workloads_for_extension(
                    spec,
                    rig_context
                        .as_ref()
                        .and_then(|context| context.package_root.as_deref()),
                    id,
                )
            })
        })
        .unwrap_or_default();

    let run_dir = RunDir::create()?;
    let output = extension_bench::run_bench_list_workflow(
        &ctx.component,
        BenchListWorkflowArgs {
            component_label: effective_id,
            component_id: ctx.component_id.clone(),
            path_override,
            settings: ctx
                .settings
                .iter()
                .filter_map(|(k, v)| match v {
                    serde_json::Value::String(s) => Some((k.clone(), s.clone())),
                    _ => None,
                })
                .collect(),
            settings_json: ctx
                .settings
                .iter()
                .filter_map(|(k, v)| match v {
                    serde_json::Value::String(_) => None,
                    other => Some((k.clone(), other.clone())),
                })
                .collect(),
            passthrough_args,
            scenario_ids: args.scenario_ids.clone(),
            extra_workloads,
        },
        &run_dir,
    )?;

    Ok((BenchOutput::List(output), 0))
}

struct ListRigContext {
    spec: RigSpec,
    package_root: Option<PathBuf>,
}

fn load_list_rig(args: &BenchListArgs) -> homeboy::Result<Option<ListRigContext>> {
    match args.rig.as_slice() {
        [] => Ok(None),
        [rig_id] => {
            let spec = rig::load(rig_id)?;
            let package_root = rig::read_source_metadata(&spec.id)
                .map(|metadata| PathBuf::from(metadata.package_path));
            Ok(Some(ListRigContext { spec, package_root }))
        }
        _ => Err(homeboy::Error::validation_invalid_argument(
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
) -> homeboy::Result<String> {
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

        return Err(homeboy::Error::validation_invalid_argument(
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

/// Resolve the candidate rig's `bench.default_baseline_rig` and, when
/// applicable, return the rewritten `[baseline, candidate]` rig list
/// the comparison path should run. Returns `None` when no expansion
/// applies — the caller falls through to its normal dispatch.
///
/// Expansion applies when ALL of the following hold:
/// - exactly one `--rig` was passed,
/// - that rig's spec declares a non-empty `bench.default_baseline_rig`,
/// - none of `--baseline` / `--ratchet` / `--ignore-default-baseline`
///   are set.
///
/// A spec that names itself as its own default baseline is rejected
/// with `validation_invalid_argument` — the auto-upgrade would loop
/// and the user almost certainly meant a different rig.
fn maybe_expand_default_baseline(args: &BenchRunArgs) -> homeboy::Result<Option<Vec<String>>> {
    if args.rig.len() != 1 {
        return Ok(None);
    }
    if args.baseline_args.baseline || args.baseline_args.ratchet || args.ignore_default_baseline {
        return Ok(None);
    }

    let candidate = &args.rig[0];
    let candidate_spec = rig::load(candidate)?;
    if args.comp.id().is_none()
        && candidate_spec
            .bench
            .as_ref()
            .map(|b| matrix::bench_component_ids(b).len() > 1)
            .unwrap_or(false)
    {
        return Ok(None);
    }
    let Some(baseline_rig_id) = candidate_spec
        .bench
        .as_ref()
        .and_then(|b| b.default_baseline_rig.clone())
    else {
        return Ok(None);
    };

    if baseline_rig_id == *candidate {
        return Err(homeboy::Error::validation_invalid_argument(
            "bench.default_baseline_rig",
            format!(
                "rig '{}' declares itself as its own default_baseline_rig; \
                 fix the rig spec or pass --ignore-default-baseline",
                candidate
            ),
            None,
            None,
        ));
    }

    Ok(Some(vec![baseline_rig_id, candidate.clone()]))
}

fn expand_bench_workload_path(
    rig_spec: &RigSpec,
    package_root: Option<&Path>,
    path: &str,
) -> PathBuf {
    let expanded = rig::expand::expand_vars(rig_spec, path);
    let expanded = match package_root {
        Some(root) => expanded.replace("${package.root}", &root.to_string_lossy()),
        None => expanded,
    };
    PathBuf::from(expanded)
}

fn bench_workloads_for_extension(
    rig_spec: &RigSpec,
    package_root: Option<&Path>,
    extension_id: &str,
) -> Vec<PathBuf> {
    rig_spec
        .bench_workloads
        .get(extension_id)
        .into_iter()
        .flat_map(|paths| paths.iter())
        .map(|path| expand_bench_workload_path(rig_spec, package_root, path))
        .collect()
}

#[cfg(test)]
mod tests;
