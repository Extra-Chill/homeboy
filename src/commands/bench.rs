use clap::Args;
use serde::Serialize;

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::engine::run_dir::RunDir;
use homeboy::extension::bench as extension_bench;
use homeboy::extension::bench::{
    aggregate_comparison, BenchCommandOutput, BenchComparisonOutput, BenchRunWorkflowArgs,
    RigBenchEntry, DEFAULT_REGRESSION_THRESHOLD_PERCENT,
};
use homeboy::extension::ExtensionCapability;
use homeboy::rig;

use super::utils::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct BenchArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Iterations per scenario (default 10). Forwarded to the runner via
    /// HOMEBOY_BENCH_ITERATIONS. Individual extensions may clamp.
    #[arg(long, default_value_t = 10)]
    iterations: u64,

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
        "--ratchet",
        "--json-summary",
        "--json",
    ];

    const HOMEBOY_VALUE_FLAGS: &[&str] = &[
        "--iterations",
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
}

pub fn run(args: BenchArgs, _global: &GlobalArgs) -> CmdResult<BenchOutput> {
    let passthrough_args = filter_homeboy_flags(&args.args);

    // No --rig: legacy single bare run. No rig pinning, no rig
    // snapshot, baseline key untouched. Identical to before this PR.
    if args.rig.is_empty() {
        let (output, exit) = run_single(&args, &passthrough_args, None)?;
        return Ok((BenchOutput::Single(output), exit));
    }

    // --rig with one value: legacy single rig-pinned run. Same shape as
    // before this PR for `bench --rig <id>` callers (single output, rig
    // snapshot embedded). Baseline flags still honored.
    if args.rig.len() == 1 {
        let rig_id = args.rig[0].clone();
        let (output, exit) = run_single(&args, &passthrough_args, Some(rig_id))?;
        return Ok((BenchOutput::Single(output), exit));
    }

    // --rig with two or more values: cross-rig comparison. Run each rig
    // in sequence, collect per-rig outputs, aggregate into a
    // BenchComparisonOutput.
    if args.baseline_args.baseline {
        return Err(homeboy::Error::validation_invalid_argument(
            "--baseline",
            "Cannot --baseline a cross-rig run; baselines are per-rig. \
             Run `homeboy bench --rig <id> --baseline` once per rig you \
             want to ratchet.",
            None,
            None,
        ));
    }
    if args.baseline_args.ratchet {
        return Err(homeboy::Error::validation_invalid_argument(
            "--ratchet",
            "Cannot --ratchet a cross-rig run; baselines are per-rig. \
             Run `homeboy bench --rig <id> --ratchet` once per rig.",
            None,
            None,
        ));
    }

    let mut entries = Vec::with_capacity(args.rig.len());
    let mut effective_component_label: Option<String> = None;

    for rig_id in &args.rig {
        let (single_output, _exit) = run_single(&args, &passthrough_args, Some(rig_id.clone()))?;
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
        .or_else(|| args.comp.id().map(|s| s.to_string()))
        .unwrap_or_else(|| "<unknown>".to_string());

    let (output, exit) = aggregate_comparison(component, args.iterations, entries);
    Ok((BenchOutput::Comparison(output), exit))
}

/// Run bench once, optionally pinned to a rig, and return the standard
/// `BenchCommandOutput` envelope. This is the unit of work that both
/// the legacy single-run path and the new cross-rig comparison path
/// share, so behavior stays identical for single-rig callers.
fn run_single(
    args: &BenchArgs,
    passthrough_args: &[String],
    rig_id_override: Option<String>,
) -> CmdResult<BenchCommandOutput> {
    let (rig_id, rig_snapshot, default_component_id) = match rig_id_override.as_deref() {
        None => (None, None, None),
        Some(rig_id) => {
            let rig_spec = rig::load(rig_id)?;
            let check_report = rig::run_check(&rig_spec)?;
            if !check_report.success {
                return Err(homeboy::Error::rig_pipeline_failed(
                    &rig_spec.id,
                    "check",
                    "rig check failed; refusing to run bench against an unhealthy rig",
                ));
            }
            let snapshot = rig::snapshot_state(&rig_spec);
            let default = rig_spec
                .bench
                .as_ref()
                .and_then(|b| b.default_component.clone());
            (Some(rig_spec.id.clone()), Some(snapshot), default)
        }
    };

    // Component resolution: explicit positional > rig.bench.default_component
    // > auto-detect from CWD (the existing PositionalComponentArgs path).
    let effective_id = match (args.comp.id(), default_component_id) {
        (Some(id), _) => id.to_string(),
        (None, Some(default)) => default,
        (None, None) => args.comp.resolve_id()?,
    };

    let ctx = execution_context::resolve(&ResolveOptions::with_capability_and_json(
        &effective_id,
        args.comp.path.clone(),
        ExtensionCapability::Bench,
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
    ))?;

    let run_dir = RunDir::create()?;

    let workflow = extension_bench::run_main_bench_workflow(
        &ctx.component,
        &ctx.source_path,
        BenchRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            // Split ctx.settings by JSON shape: strings flow through the
            // legacy `settings` channel (string-coerced for downstream
            // consumers), non-string types (objects, arrays, numbers,
            // bools, nulls) flow through `settings_json` so type is
            // preserved end-to-end. Without the split, non-string values
            // would be `.to_string()`'d into JSON literals and downstream
            // `jq -c '.field'` extractions would surface a string-encoded
            // JSON object instead of the actual object.
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
            iterations: args.iterations,
            baseline_flags: homeboy::engine::baseline::BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
            regression_threshold_percent: args.regression_threshold,
            json_summary: args.json_summary,
            passthrough_args: passthrough_args.to_vec(),
            rig_id: rig_id.clone(),
            shared_state: None,
            concurrency: 1,
        },
        &run_dir,
    )?;

    Ok(extension_bench::from_main_workflow_with_rig(
        workflow,
        rig_snapshot,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_strips_boolean_flags() {
        let args = vec!["--ratchet".to_string(), "--filter=Scenario".to_string()];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=Scenario"]);
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
