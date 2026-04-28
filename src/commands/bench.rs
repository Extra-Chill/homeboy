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

    /// Only run matching benchmark scenario ids. Repeat to select multiple.
    #[arg(long = "scenario", value_name = "SCENARIO_ID")]
    scenario_ids: Vec<String>,

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
        "--scenario",
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
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use clap::Parser;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Minimal CLI wrapper to exercise clap parsing of `BenchArgs`.
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        bench: BenchArgs,
    }

    fn write_bench_extension(home: &TempDir) {
        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join("nodejs");
        fs::create_dir_all(&extension_dir).expect("mkdir extension");
        fs::write(
            extension_dir.join("nodejs.json"),
            r#"{
                "name": "Node.js",
                "version": "0.0.0",
                "bench": { "extension_script": "bench-runner.sh" }
            }"#,
        )
        .expect("write extension manifest");

        let script_path = extension_dir.join("bench-runner.sh");
        fs::write(
            &script_path,
            r#"#!/bin/sh
if [ -n "$HOMEBOY_BENCH_EXTRA_WORKLOADS" ]; then
  all_scenarios=""
  old_ifs="$IFS"
  IFS=":"
  for workload in $HOMEBOY_BENCH_EXTRA_WORKLOADS; do
    name="$(basename "$workload")"
    name="${name%%.bench.*}"
    name="${name%.*}"
    if [ -n "$all_scenarios" ]; then
      all_scenarios="$all_scenarios $name"
    else
      all_scenarios="$name"
    fi
  done
  IFS="$old_ifs"
else
  all_scenarios="in-tree slow"
fi

# Rig-declared workload selection is owned by Homeboy core because the core
# process builds HOMEBOY_BENCH_EXTRA_WORKLOADS. This intentionally ignores
# HOMEBOY_BENCH_SCENARIOS when extra workloads are present, matching the real
# Node runner class that caused #1843.
if [ -n "$HOMEBOY_BENCH_EXTRA_WORKLOADS" ] || [ "$HOMEBOY_BENCH_LIST_ONLY" = "1" ] || [ -z "$HOMEBOY_BENCH_SCENARIOS" ]; then
  selected="$all_scenarios"
else
  selected=$(printf '%s' "$HOMEBOY_BENCH_SCENARIOS" | tr ',' ' ')
fi

cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{
  "component_id": "$HOMEBOY_COMPONENT_ID",
  "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0},
  "scenarios": [
JSON

comma=""
for scenario in $selected; do
  cat >> "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
    $comma{ "id": "$scenario", "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0}, "metrics": { "p95_ms": 1.0 } }
JSON
  comma=",
"
done

cat >> "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
  ],
  "metric_policies": { "p95_ms": { "direction": "lower_is_better" } }
}
JSON
"#,
        )
        .expect("write bench script");

        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions).expect("chmod script");
        }
    }

    fn write_registered_component(home: &TempDir, component_id: &str, path: &std::path::Path) {
        let component_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&component_dir).expect("mkdir components");
        fs::write(
            component_dir.join(format!("{}.json", component_id)),
            serde_json::json!({
                "id": component_id,
                "local_path": path,
                "extensions": { "nodejs": {} }
            })
            .to_string(),
        )
        .expect("write component");
    }

    fn write_rig(home: &TempDir, rig_id: &str, component_id: &str, path: &std::path::Path) {
        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        fs::write(
            rig_dir.join(format!("{}.json", rig_id)),
            format!(
                r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "extensions": {{ "nodejs": {{}} }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }},
                    "bench_workloads": {{ "nodejs": [
                        "${{components.{component_id}.path}}/rig-extra.bench.js",
                        "${{components.{component_id}.path}}/rig-slow.bench.mjs"
                    ] }}
                }}"#,
                path.display()
            ),
        )
        .expect("write rig");
    }

    fn list_args(component: Option<&str>, rig: Vec<String>) -> BenchListArgs {
        BenchListArgs {
            comp: PositionalComponentArgs {
                component: component.map(str::to_string),
                path: None,
            },
            rig,
            scenario_ids: Vec::new(),
            setting_args: SettingArgs::default(),
            args: Vec::new(),
        }
    }

    fn run_args(component: Option<&str>, rig: Vec<String>, scenario_ids: Vec<String>) -> BenchArgs {
        BenchArgs {
            command: None,
            run: BenchRunArgs {
                comp: PositionalComponentArgs {
                    component: component.map(str::to_string),
                    path: None,
                },
                iterations: 1,
                runs: 1,
                shared_state: None,
                concurrency: 1,
                baseline_args: BaselineArgs {
                    baseline: false,
                    ignore_baseline: true,
                    ratchet: false,
                },
                regression_threshold: 5.0,
                setting_args: SettingArgs::default(),
                args: Vec::new(),
                _json: HiddenJsonArgs::default(),
                json_summary: false,
                rig,
                scenario_ids,
                ignore_default_baseline: false,
            },
        }
    }

    #[test]
    fn filter_strips_boolean_flags() {
        let args = vec!["--ratchet".to_string(), "--filter=Scenario".to_string()];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=Scenario"]);
    }

    #[test]
    fn parses_bench_list_rig_flag() {
        let cli = TestCli::try_parse_from(["bench", "list", "--rig", "studio-bfb"])
            .expect("bench list --rig should parse");

        match cli.bench.command.expect("list command") {
            BenchCommand::List(args) => assert_eq!(args.rig, vec!["studio-bfb".to_string()]),
        }
    }

    #[test]
    fn parses_repeated_scenario_flags() {
        let cli = TestCli::try_parse_from([
            "bench",
            "homeboy",
            "--scenario",
            "studio-agent-runtime",
            "--scenario",
            "wp-admin-load",
        ])
        .expect("bench --scenario should parse");

        assert_eq!(
            cli.bench.run.scenario_ids,
            vec![
                "studio-agent-runtime".to_string(),
                "wp-admin-load".to_string()
            ]
        );
    }

    #[test]
    fn run_list_uses_rig_default_component_and_workloads() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_rig(home, "studio-bfb", "studio", component_dir.path());

            let (output, exit_code) = run_list(&list_args(None, vec!["studio-bfb".to_string()]))
                .expect("rig bench list should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::List(result) => {
                    assert_eq!(result.component, "studio");
                    assert_eq!(result.component_id, "studio");
                    assert_eq!(result.count, 2);
                    assert_eq!(result.scenarios[0].id, "rig-extra");
                }
                _ => panic!("expected list output"),
            }
        });
    }

    #[test]
    fn run_list_preserves_registered_component_path() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_registered_component(home, "studio", component_dir.path());

            let (output, exit_code) = run_list(&list_args(Some("studio"), Vec::new()))
                .expect("plain bench list should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::List(result) => {
                    assert_eq!(result.component, "studio");
                    assert_eq!(result.component_id, "studio");
                    assert_eq!(result.count, 2);
                    assert_eq!(result.scenarios[0].id, "in-tree");
                }
                _ => panic!("expected list output"),
            }
        });
    }

    #[test]
    fn run_list_filters_selected_scenario() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_registered_component(home, "studio", component_dir.path());

            let mut args = list_args(Some("studio"), Vec::new());
            args.scenario_ids = vec!["slow".to_string()];
            let (output, exit_code) = run_list(&args).expect("plain bench list should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::List(result) => {
                    assert_eq!(result.count, 1);
                    assert_eq!(result.scenarios[0].id, "slow");
                }
                _ => panic!("expected list output"),
            }
        });
    }

    #[test]
    fn run_selects_single_scenario() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_registered_component(home, "studio", component_dir.path());

            let (output, exit_code) = run(
                run_args(Some("studio"), Vec::new(), vec!["slow".to_string()]),
                &GlobalArgs {},
            )
            .expect("selected bench should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::Single(result) => {
                    let scenarios = result.results.expect("results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    assert_eq!(scenarios[0].id, "slow");
                }
                _ => panic!("expected single output"),
            }
        });
    }

    #[test]
    fn parses_rig_run_options_without_component() {
        let cli = TestCli::try_parse_from([
            "bench",
            "--rig",
            "studio-agent-sdk,studio-agent-pi",
            "--scenario",
            "studio-agent-runtime",
            "--runs",
            "3",
            "--iterations",
            "1",
        ])
        .expect("rig bench options without component should parse");

        assert_eq!(
            cli.bench.run.rig,
            vec!["studio-agent-sdk", "studio-agent-pi"]
        );
        assert_eq!(cli.bench.run.scenario_ids, vec!["studio-agent-runtime"]);
        assert_eq!(cli.bench.run.runs, 3);
        assert_eq!(cli.bench.run.iterations, 1);
        assert!(cli.bench.run.comp.id().is_none());
    }

    #[test]
    fn run_selects_multiple_scenarios() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_registered_component(home, "studio", component_dir.path());

            let (output, exit_code) = run(
                run_args(
                    Some("studio"),
                    Vec::new(),
                    vec!["in-tree".to_string(), "slow".to_string()],
                ),
                &GlobalArgs {},
            )
            .expect("selected bench should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::Single(result) => {
                    let scenario_ids: Vec<String> = result
                        .results
                        .expect("results")
                        .scenarios
                        .into_iter()
                        .map(|scenario| scenario.id)
                        .collect();
                    assert_eq!(
                        scenario_ids,
                        vec!["in-tree".to_string(), "slow".to_string()]
                    );
                }
                _ => panic!("expected single output"),
            }
        });
    }

    #[test]
    fn unknown_scenario_reports_discovered_ids() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_registered_component(home, "studio", component_dir.path());

            let err = match run(
                run_args(Some("studio"), Vec::new(), vec!["missing".to_string()]),
                &GlobalArgs {},
            ) {
                Ok(_) => panic!("unknown scenario should fail"),
                Err(err) => err,
            };
            let message = err.to_string();

            assert!(message.contains("missing"), "got: {}", message);
            assert!(message.contains("in-tree"), "got: {}", message);
            assert!(message.contains("slow"), "got: {}", message);
        });
    }

    #[test]
    fn cross_rig_run_passes_selector_to_each_rig() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_a = tempfile::TempDir::new().expect("component a");
            let component_b = tempfile::TempDir::new().expect("component b");
            write_rig(home, "rig-a", "studio", component_a.path());
            write_rig(home, "rig-b", "studio", component_b.path());

            let (output, exit_code) = run(
                run_args(
                    None,
                    vec!["rig-a".to_string(), "rig-b".to_string()],
                    vec!["rig-slow".to_string()],
                ),
                &GlobalArgs {},
            )
            .expect("cross-rig selected bench should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::Comparison(result) => {
                    assert_eq!(result.rigs.len(), 2);
                    for rig in result.rigs {
                        let scenarios = rig.results.expect("rig results").scenarios;
                        assert_eq!(scenarios.len(), 1);
                        assert_eq!(scenarios[0].id, "rig-slow");
                    }
                }
                _ => panic!("expected comparison output"),
            }
        });
    }

    #[test]
    fn single_rig_selector_filters_extra_workloads_before_execution() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_rig(home, "rig-a", "studio", component_dir.path());

            let (output, exit_code) = run(
                run_args(
                    None,
                    vec!["rig-a".to_string()],
                    vec!["rig-slow".to_string()],
                ),
                &GlobalArgs {},
            )
            .expect("single-rig selected bench should run");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::Single(result) => {
                    assert_eq!(result.iterations, 1);
                    let scenarios = result.results.expect("results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    assert_eq!(scenarios[0].id, "rig-slow");
                    assert_eq!(scenarios[0].iterations, 1);
                }
                _ => panic!("expected single output"),
            }
        });
    }

    #[test]
    fn single_rig_runs_preserve_run_level_summaries_after_selection() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_rig(home, "rig-a", "studio", component_dir.path());
            let mut args = run_args(
                None,
                vec!["rig-a".to_string()],
                vec!["rig-slow".to_string()],
            );
            args.run.runs = 3;

            let (output, exit_code) = run(args, &GlobalArgs {})
                .expect("single-rig selected bench should run multiple runs");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::Single(result) => {
                    let scenarios = result.results.expect("results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    let runs = scenarios[0].runs.as_ref().expect("per-run metrics");
                    assert_eq!(runs.len(), 3);
                    assert!(scenarios[0].runs_summary.as_ref().is_some_and(|summary| {
                        summary.get("p95_ms").is_some_and(|metric| metric.n == 3)
                    }));
                }
                _ => panic!("expected single output"),
            }
        });
    }

    #[test]
    fn cross_rig_runs_preserve_run_level_summaries_after_selection() {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_a = tempfile::TempDir::new().expect("component a");
            let component_b = tempfile::TempDir::new().expect("component b");
            write_rig(home, "rig-a", "studio", component_a.path());
            write_rig(home, "rig-b", "studio", component_b.path());
            let mut args = run_args(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                vec!["rig-slow".to_string()],
            );
            args.run.runs = 3;

            let (output, exit_code) = run(args, &GlobalArgs {})
                .expect("cross-rig selected bench should run multiple runs");

            assert_eq!(exit_code, 0);
            match output {
                BenchOutput::Comparison(result) => {
                    assert_eq!(result.rigs.len(), 2);
                    assert!(result
                        .summary
                        .iter()
                        .all(|summary| summary.rows.iter().all(|row| row.n == Some(3))));
                    for rig in result.rigs {
                        let scenarios = rig.results.expect("rig results").scenarios;
                        assert_eq!(scenarios.len(), 1);
                        assert_eq!(scenarios[0].id, "rig-slow");
                        assert_eq!(scenarios[0].runs.as_ref().expect("runs").len(), 3);
                    }
                }
                _ => panic!("expected comparison output"),
            }
        });
    }

    #[test]
    fn run_list_requires_rig_default_component_when_component_omitted() {
        with_isolated_home(|home| {
            let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
            fs::create_dir_all(&rig_dir).expect("mkdir rigs");
            fs::write(rig_dir.join("empty.json"), r#"{ "bench": {} }"#).expect("write rig");

            let err = match run_list(&list_args(None, vec!["empty".to_string()])) {
                Ok(_) => panic!("missing default component should error"),
                Err(err) => err,
            };
            let message = err.to_string();
            assert!(
                message.contains("bench.default_component"),
                "expected default-component error, got: {}",
                message
            );
        });
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
            artifacts: Vec::new(),
            results: None,
            gate_failures: Vec::new(),
            baseline_comparison: None,
            hints: None,
            rig_state: None,
            failure: None,
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
