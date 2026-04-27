use clap::{Args, Subcommand};
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

    /// Skip auto-upgrading single-rig runs into a comparison even when
    /// the rig spec declares `bench.default_baseline_rig`. Use with
    /// `--baseline` / `--ratchet` against a rig that normally
    /// auto-pairs, or to bench the candidate alone.
    #[arg(long)]
    ignore_default_baseline: bool,
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
        "--runs",
        "--shared-state",
        "--concurrency",
        "--regression-threshold",
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

    let mut entries = Vec::with_capacity(run_args.rig.len());
    let mut effective_component_label: Option<String> = None;

    for rig_id in &run_args.rig {
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
            results: single_output.results,
            rig_state: single_output.rig_state,
        });
    }

    let component = effective_component_label
        .or_else(|| run_args.comp.id().map(|s| s.to_string()))
        .unwrap_or_else(|| "<unknown>".to_string());

    let (output, exit) = aggregate_comparison(component, run_args.iterations, entries);
    Ok((BenchOutput::Comparison(output), exit))
}

fn run_list(args: &BenchListArgs) -> CmdResult<BenchOutput> {
    let passthrough_args = filter_homeboy_flags(&args.args);
    let effective_id = args.comp.resolve_id()?;

    let ctx = execution_context::resolve(&ResolveOptions::with_capability_and_json(
        &effective_id,
        args.comp.path.clone(),
        ExtensionCapability::Bench,
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
    ))?;

    let run_dir = RunDir::create()?;
    let output = extension_bench::run_bench_list_workflow(
        &ctx.component,
        BenchListWorkflowArgs {
            component_label: effective_id,
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
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
            extra_workloads: Vec::new(),
        },
        &run_dir,
    )?;

    Ok((BenchOutput::List(output), 0))
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
mod tests {
    use super::*;
    use clap::Parser;

    /// Minimal CLI wrapper to exercise clap parsing of `BenchArgs`.
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        bench: BenchArgs,
    }

    #[test]
    fn filter_strips_boolean_flags() {
        let args = vec!["--ratchet".to_string(), "--filter=Scenario".to_string()];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=Scenario"]);
    }

    #[test]
    fn bench_workloads_for_extension_filters_and_expands_paths() {
        std::env::set_var("HOMEBOY_TEST_BENCH_ROOT", "/tmp/private-benches");
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "studio",
                "components": {
                    "playground": { "path": "/tmp/playground" }
                },
                "bench_workloads": {
                    "wordpress": [
                        "${env.HOMEBOY_TEST_BENCH_ROOT}/cold-boot.php",
                        "${components.playground.path}/fixtures/wc-loaded.php"
                    ],
                    "nodejs": ["/tmp/node-only.bench.ts"]
                }
            }"#,
        )
        .expect("parse rig spec");

        let workloads = bench_workloads_for_extension(&rig_spec, None, "wordpress");

        assert_eq!(
            workloads,
            vec![
                PathBuf::from("/tmp/private-benches/cold-boot.php"),
                PathBuf::from("/tmp/playground/fixtures/wc-loaded.php"),
            ]
        );
        assert!(bench_workloads_for_extension(&rig_spec, None, "rust").is_empty());
    }

    #[test]
    fn bench_workloads_for_extension_expands_package_root_when_available() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "studio-agent-sdk",
                "bench_workloads": {
                    "nodejs": [
                        "${package.root}/bench/studio-agent-runtime.bench.mjs",
                        "/tmp/absolute.bench.mjs"
                    ]
                }
            }"#,
        )
        .expect("parse rig spec");
        let package = PathBuf::from("/tmp/homeboy-rigs/Automattic/studio");

        let workloads = bench_workloads_for_extension(&rig_spec, Some(&package), "nodejs");

        assert_eq!(
            workloads,
            vec![
                PathBuf::from(
                    "/tmp/homeboy-rigs/Automattic/studio/bench/studio-agent-runtime.bench.mjs"
                ),
                PathBuf::from("/tmp/absolute.bench.mjs"),
            ]
        );
    }

    #[test]
    fn bench_workloads_for_extension_leaves_package_root_unexpanded_without_metadata() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "manual",
                "bench_workloads": {
                    "nodejs": ["${package.root}/bench/manual.bench.mjs"]
                }
            }"#,
        )
        .expect("parse rig spec");

        let workloads = bench_workloads_for_extension(&rig_spec, None, "nodejs");

        assert_eq!(
            workloads,
            vec![PathBuf::from("${package.root}/bench/manual.bench.mjs")]
        );
    }

    #[test]
    fn filter_strips_all_boolean_flags() {
        let args = vec![
            "--baseline".to_string(),
            "--ignore-baseline".to_string(),
            "--ratchet".to_string(),
            "--json-summary".to_string(),
            "--json".to_string(),
        ];
        assert!(filter_homeboy_flags(&args).is_empty());
    }

    #[test]
    fn filter_strips_iterations_space_form() {
        let args = vec![
            "--iterations".to_string(),
            "50".to_string(),
            "--filter=Scenario".to_string(),
        ];
        assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);
    }

    #[test]
    fn filter_strips_iterations_equals_form() {
        let args = vec!["--iterations=50".to_string(), "--keep".to_string()];
        assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
    }

    #[test]
    fn parses_shared_state_and_concurrency_flags() {
        let cli = TestCli::try_parse_from([
            "bench",
            "homeboy",
            "--shared-state",
            "/tmp/foo",
            "--concurrency",
            "4",
        ])
        .expect("shared-state and concurrency flags should parse");

        assert_eq!(cli.bench.run.shared_state, Some(PathBuf::from("/tmp/foo")));
        assert_eq!(cli.bench.run.concurrency, 4);
    }

    #[test]
    fn filter_strips_shared_state_and_concurrency_forms() {
        let args = vec![
            "--shared-state".to_string(),
            "/tmp/foo".to_string(),
            "--concurrency".to_string(),
            "4".to_string(),
            "--filter=Scenario".to_string(),
        ];
        assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

        let args = vec![
            "--shared-state=/tmp/foo".to_string(),
            "--concurrency=4".to_string(),
            "--keep".to_string(),
        ];
        assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
    }

    #[test]
    fn filter_strips_regression_threshold_forms() {
        let args = vec![
            "--regression-threshold".to_string(),
            "10".to_string(),
            "--keep".to_string(),
        ];
        assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);

        let args = vec![
            "--regression-threshold=10".to_string(),
            "--keep".to_string(),
        ];
        assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
    }

    #[test]
    fn filter_preserves_unknown_flags() {
        let args = vec![
            "--filter=Scenario".to_string(),
            "--verbose".to_string(),
            "extra".to_string(),
        ];
        assert_eq!(filter_homeboy_flags(&args), args);
    }

    #[test]
    fn filter_handles_empty() {
        assert!(filter_homeboy_flags(&[]).is_empty());
    }

    #[test]
    fn filter_handles_mixed() {
        let args = vec![
            "--ratchet".to_string(),
            "--iterations".to_string(),
            "25".to_string(),
            "--filter=hot_path".to_string(),
            "--regression-threshold=7.5".to_string(),
            "--verbose".to_string(),
        ];
        assert_eq!(
            filter_homeboy_flags(&args),
            vec!["--filter=hot_path", "--verbose"]
        );
    }

    #[test]
    fn bench_output_single_serializes_without_wrapper_key() {
        // Backcompat: single-rig and bare-bench output must serialize
        // identically to the pre-cross-rig shape (no top-level
        // discriminator field). The `untagged` enum representation
        // gives us that for free, but pin it with a test so a future
        // refactor can't quietly break consumers.
        let single = BenchCommandOutput {
            passed: true,
            status: "passed".to_string(),
            component: "studio".to_string(),
            exit_code: 0,
            iterations: 10,
            results: None,
            baseline_comparison: None,
            hints: None,
            rig_state: None,
        };
        let value = serde_json::to_value(BenchOutput::Single(single)).unwrap();
        assert!(value.get("comparison").is_none());
        assert_eq!(value.get("passed"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(
            value.get("component"),
            Some(&serde_json::Value::String("studio".to_string()))
        );
    }

    #[test]
    fn bench_output_comparison_serializes_with_discriminator() {
        let (cmp, _) = aggregate_comparison("studio".to_string(), 10, Vec::new());
        let value = serde_json::to_value(BenchOutput::Comparison(cmp)).unwrap();
        assert_eq!(
            value.get("comparison"),
            Some(&serde_json::Value::String("cross_rig".to_string()))
        );
    }
}

#[cfg(test)]
#[path = "../../tests/core/rig/bench_default_baseline_dispatch_test.rs"]
mod bench_default_baseline_dispatch_test;
