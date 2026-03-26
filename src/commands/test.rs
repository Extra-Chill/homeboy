use clap::Args;

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::engine::run_dir::RunDir;
use homeboy::extension::test as extension_test;
use homeboy::extension::test::{detect_test_drift, report, TestCommandOutput, TestRunWorkflowArgs};
use homeboy::extension::ExtensionCapability;

use super::utils::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct TestArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Skip linting before running tests
    #[arg(long)]
    skip_lint: bool,

    /// Collect code coverage (requires xdebug/pcov for PHP, cargo-tarpaulin for Rust)
    #[arg(long)]
    coverage: bool,

    /// Minimum coverage percentage — fail if below this threshold (implies --coverage)
    #[arg(long, value_name = "PERCENT")]
    coverage_min: Option<f64>,

    #[command(flatten)]
    baseline_args: BaselineArgs,

    /// Auto-update baseline when test results improve (ratchet forward)
    #[arg(long)]
    ratchet: bool,

    /// Analyze test failures — cluster by root cause and suggest fixes
    #[arg(long)]
    analyze: bool,

    /// Detect test drift — cross-reference production changes with test files
    #[arg(long)]
    drift: bool,

    /// Write fixes to disk for workflows that support it
    #[arg(long)]
    write: bool,

    /// Git ref to compare against for drift detection (tag, commit, branch)
    #[arg(long, value_name = "REF", default_value = "HEAD~10")]
    since: String,

    /// Limit test execution to files changed since this git ref (PR impact scope)
    #[arg(long, value_name = "REF")]
    changed_since: Option<String>,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Additional arguments to pass to the test runner (must follow --)
    #[arg(last = true)]
    args: Vec<String>,

    #[command(flatten)]
    _json: HiddenJsonArgs,

    /// Print compact machine-readable summary (for CI wrappers)
    #[arg(long)]
    json_summary: bool,
}

/// Filter out homeboy-owned flags from trailing args before passing to extension scripts.
///
/// Clap's `trailing_var_arg = true` + `allow_hyphen_values = true` captures all arguments
/// after the positional component arg — including flags that Clap also parsed into named
/// fields. This means `--analyze`, `--drift`, etc. end up in both `args.analyze = true`
/// AND `args.args = ["--analyze"]`. The extension test runner passes `args.args` through
/// to the underlying tool (e.g. PHPUnit), which then fails on unknown flags.
///
/// This function strips homeboy-owned flags so only genuine passthrough args (like
/// `--filter=TestName`) reach the extension script.
fn filter_homeboy_flags(args: &[String]) -> Vec<String> {
    // Homeboy-owned boolean flags that should never reach the extension runner
    const HOMEBOY_FLAGS: &[&str] = &[
        "--analyze",
        "--drift",
        "--write",
        "--json-summary",
        "--baseline",
        "--ignore-baseline",
        "--ratchet",
        "--skip-lint",
        "--coverage",
        "--json",
    ];

    // Homeboy-owned flags that take a value (--flag value or --flag=value)
    const HOMEBOY_VALUE_FLAGS: &[&str] = &[
        "--coverage-min",
        "--since",
        "--changed-since",
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

        // Check boolean flags (exact match)
        if HOMEBOY_FLAGS.contains(&arg.as_str()) {
            continue;
        }

        // Check value flags: --flag=value (single arg) or --flag value (two args)
        let is_value_flag = HOMEBOY_VALUE_FLAGS.iter().any(|f| {
            if arg.starts_with(&format!("{}=", f)) {
                return true; // --flag=value form, skip this arg only
            }
            if arg == *f {
                skip_next = true; // --flag value form, skip this and next
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

pub fn run(args: TestArgs, _global: &GlobalArgs) -> CmdResult<TestCommandOutput> {
    let ctx = execution_context::resolve(&ResolveOptions::with_capability(
        args.comp.id(),
        args.comp.path.clone(),
        ExtensionCapability::Test,
        args.setting_args.setting.clone(),
    ))?;

    // Drift detection mode — delegate to core drift workflow (read-only)
    // Fixes are owned by `homeboy refactor --from test --write`.
    if args.drift {
        let result = detect_test_drift(args.comp.id(), &ctx.component, &args.since)?;
        return Ok(report::from_drift_workflow(result));
    }

    // Main test workflow — delegate to core
    let run_dir = RunDir::create()?;
    let passthrough_args = filter_homeboy_flags(&args.args);
    let workflow = extension_test::run_main_test_workflow(
        &ctx.component,
        &ctx.source_path,
        TestRunWorkflowArgs {
            component_label: args.comp.component.clone(),
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
            skip_lint: args.skip_lint,
            coverage: args.coverage,
            coverage_min: args.coverage_min,
            analyze: args.analyze,
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.ratchet,
            changed_since: args.changed_since.clone(),
            json_summary: args.json_summary,
            passthrough_args: passthrough_args.clone(),
        },
        &run_dir,
    )?;

    Ok(report::from_main_workflow(workflow))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::component::Component;
    use homeboy::refactor::plan::{test_refactor_request, TestSourceOptions};
    use std::path::PathBuf;

    #[test]
    fn filter_strips_boolean_flags() {
        let args = vec!["--analyze".to_string(), "--filter=SomeTest".to_string()];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_multiple_boolean_flags() {
        let args = vec![
            "--analyze".to_string(),
            "--drift".to_string(),
            "--baseline".to_string(),
            "--ignore-baseline".to_string(),
            "--ratchet".to_string(),
            "--skip-lint".to_string(),
            "--coverage".to_string(),
            "--write".to_string(),
            "--json".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_strips_value_flags_space_separated() {
        let args = vec![
            "--since".to_string(),
            "v0.36.0".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);

        let args = vec![
            "--changed-since".to_string(),
            "origin/main".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_value_flags_equals_form() {
        let args = vec![
            "--since=v0.36.0".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_coverage_min() {
        let args = vec![
            "--coverage-min".to_string(),
            "80".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_setting() {
        let args = vec![
            "--setting".to_string(),
            "database_type=mysql".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_preserves_unknown_flags() {
        let args = vec![
            "--filter=SomeTest".to_string(),
            "--group".to_string(),
            "ajax".to_string(),
            "--verbose".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(args, result);
    }

    #[test]
    fn filter_handles_empty() {
        let result = filter_homeboy_flags(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_handles_mixed() {
        let args = vec![
            "--analyze".to_string(),
            "--skip-lint".to_string(),
            "--since".to_string(),
            "v0.35.0".to_string(),
            "--filter=FlowAbilities".to_string(),
            "--coverage-min=80".to_string(),
            "--verbose".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=FlowAbilities", "--verbose"]);
    }

    #[test]
    fn filter_strips_path_flag() {
        let args = vec![
            "--path".to_string(),
            "/tmp/checkout".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_json_summary_flag() {
        let args = vec![
            "--json-summary".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn test_fix_builds_canonical_refactor_request() {
        let component = Component::new(
            "demo".to_string(),
            "/tmp/demo".to_string(),
            String::new(),
            None,
        );

        let request = test_refactor_request(
            component.clone(),
            PathBuf::from("/tmp/demo"),
            vec![("runner".to_string(), "ci".to_string())],
            TestSourceOptions {
                selected_files: Some(vec!["tests/demo_test.rs".to_string()]),
                skip_lint: true,
                script_args: vec!["--filter=DemoTest".to_string()],
            },
            true,
        );

        assert_eq!(request.component.id, component.id);
        assert_eq!(request.sources, vec!["test".to_string()]);
        assert!(request.write);
        assert_eq!(request.settings.len(), 1);
        assert!(request.lint.selected_files.is_none());
        assert_eq!(request.test.selected_files.as_ref().unwrap().len(), 1);
        assert!(request.test.skip_lint);
    }
}
