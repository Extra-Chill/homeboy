use clap::Args;

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::engine::run_dir::RunDir;
use homeboy::extension::bench as extension_bench;
use homeboy::extension::bench::{
    from_main_workflow, BenchCommandOutput, BenchRunWorkflowArgs,
    DEFAULT_REGRESSION_THRESHOLD_PERCENT,
};
use homeboy::extension::ExtensionCapability;

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

    /// Auto-update the baseline when scenarios improve (p95 got faster)
    #[arg(long)]
    ratchet: bool,

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

pub fn run(args: BenchArgs, _global: &GlobalArgs) -> CmdResult<BenchCommandOutput> {
    let effective_id = args.comp.resolve_id()?;

    let ctx = execution_context::resolve(&ResolveOptions::with_capability(
        &effective_id,
        args.comp.path.clone(),
        ExtensionCapability::Bench,
        args.setting_args.setting.clone(),
    ))?;

    let run_dir = RunDir::create()?;
    let passthrough_args = filter_homeboy_flags(&args.args);

    let workflow = extension_bench::run_main_bench_workflow(
        &ctx.component,
        &ctx.source_path,
        BenchRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            settings: ctx
                .settings
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        },
                    )
                })
                .collect(),
            iterations: args.iterations,
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.ratchet,
            regression_threshold_percent: args.regression_threshold,
            json_summary: args.json_summary,
            passthrough_args,
        },
        &run_dir,
    )?;

    Ok(from_main_workflow(workflow))
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

        let args = vec!["--regression-threshold=10".to_string(), "--keep".to_string()];
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
}
