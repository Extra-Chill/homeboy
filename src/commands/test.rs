use clap::Args;

use homeboy::core::ci_profile::{self, CiResolvedJob};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::test as extension_test;
use homeboy::core::extension::test::{
    detect_test_drift, report, run_self_check_test_workflow_with_progress, TestCommandOutput,
    TestRunWorkflowArgs,
};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    finding_records_from_failure_clusters, finding_records_from_test_analysis_input,
    merge_metadata, ActiveObservation, NewRunRecord, RunStatus,
};
use std::path::{Path, PathBuf};

use super::source_command::{resolve_ci_job_for_command, resolve_source_context};
use super::utils::args::{
    filter_passthrough_args, BaselineArgs, ExtensionOverrideArgs, PassthroughCommand,
    PositionalComponentArgs, SettingArgs,
};
use super::utils::observed_workflow::ObservedWorkflowRunner;
use super::{CmdResult, GlobalArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
};
use homeboy::core::validation_progress::validation_progress_metadata;

const TEST_CHANGED_SINCE_LAB_UNSUPPORTED_REASON: &str = "`test --changed-since` is not Lab-portable yet because changed-since test selection depends on git base refs that the current Lab workspace sync may not have fetched.";

#[derive(Args)]
pub struct TestArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Skip linting before running tests
    #[arg(long)]
    pub skip_lint: bool,

    /// Collect code coverage when the selected extension supports it
    #[arg(long)]
    pub coverage: bool,

    /// Minimum coverage percentage — fail if below this threshold (implies --coverage)
    #[arg(long, value_name = "PERCENT")]
    pub coverage_min: Option<f64>,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Analyze test failures — cluster by root cause and suggest fixes
    #[arg(long)]
    pub analyze: bool,

    /// Detect test drift — cross-reference production changes with test files
    #[arg(long)]
    pub drift: bool,

    /// Write fixes to disk for workflows that support it
    #[arg(long)]
    pub write: bool,

    /// Git ref to compare against for drift detection (tag, commit, branch)
    #[arg(long, value_name = "REF", default_value = "HEAD~10")]
    pub since: String,

    /// Limit test execution to files changed since this git ref (PR impact scope)
    #[arg(long, value_name = "REF")]
    pub changed_since: Option<String>,

    #[arg(skip)]
    pub precomputed_changed_files: Option<Vec<String>>,

    /// Run using env and passthrough args from a single extension-declared CI test job.
    #[arg(long, value_name = "ID", conflicts_with = "drift")]
    pub ci_job: Option<String>,

    #[command(flatten)]
    pub setting_args: SettingArgs,

    /// Additional arguments to pass to the test runner (must follow --)
    #[arg(last = true)]
    pub args: Vec<String>,

    /// Print compact machine-readable summary (for CI wrappers)
    #[arg(long)]
    pub json_summary: bool,
}

impl TestArgs {
    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(CommandJsonFamily::Quality, output_file_mode)
    }

    pub(crate) fn lab_contract(&self) -> LabCommandContract {
        if self.changed_since.is_none() {
            LabCommandContract::portable("test", self.write.then_some("--write"), true, &[])
                .release_gate()
        } else {
            LabCommandContract::local_only("test", TEST_CHANGED_SINCE_LAB_UNSUPPORTED_REASON)
        }
    }
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
    filter_passthrough_args(PassthroughCommand::Test, args)
}

pub fn run(args: TestArgs, _global: &GlobalArgs) -> CmdResult<TestCommandOutput> {
    let source_ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        None,
    )?;
    let cli_passthrough_args = filter_homeboy_flags(&args.args);

    if !args.drift
        && args.ci_job.is_none()
        && cli_passthrough_args.is_empty()
        && source_ctx.component.has_script(ExtensionCapability::Test)
    {
        let runner =
            ObservedWorkflowRunner::create(format!("test {} self-check", source_ctx.component_id))?;
        let observation = start_test_observation(
            &source_ctx.component_id,
            &source_ctx.source_path,
            &args,
            "self-check",
            Some(runner.run_dir()),
        );
        let workflow = run_self_check_test_workflow_with_progress(
            &source_ctx.component,
            &source_ctx.source_path,
            source_ctx.component_id.clone(),
            args.json_summary,
            Some(runner.run_dir()),
            observation.as_ref().map(|observation| &observation.active),
        );

        let workflow = runner.finish(
            observation,
            workflow,
            |observation, workflow| finish_test_observation(Some(observation), workflow),
            |observation, error| finish_test_observation_error(Some(observation), error),
        )?;

        return Ok(report::from_main_workflow(workflow));
    }

    let ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        Some(ExtensionCapability::Test),
    )?;
    let effective_id = ctx.component_id.clone();
    let ci_job = resolve_ci_job_for_command(args.ci_job.as_deref(), &ctx.component, "test")?;

    // Drift detection mode — delegate to core drift workflow (read-only)
    // Fixes are owned by `homeboy refactor --from test --write`.
    if args.drift {
        let observation =
            start_test_observation(&ctx.component_id, &ctx.source_path, &args, "drift", None);
        let result = detect_test_drift(&effective_id, &ctx.component, &args.since);
        let result = match result {
            Ok(result) => {
                finish_test_drift_observation(observation, &result);
                result
            }
            Err(error) => {
                finish_test_observation_error(observation, &error);
                return Err(error);
            }
        };
        return Ok(report::from_drift_workflow(result));
    }

    // Main test workflow — delegate to core
    let runner = ObservedWorkflowRunner::create(format!("test {}", effective_id))?;
    let observation = start_test_observation(
        &ctx.component_id,
        &ctx.source_path,
        &args,
        "test",
        Some(runner.run_dir()),
    );
    let mut passthrough_args = ci_job_passthrough_args(ci_job.as_ref());
    passthrough_args.extend(cli_passthrough_args);
    let workflow = extension_test::run_main_test_workflow(
        &ctx.component,
        &ctx.source_path,
        TestRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            settings: ctx.resolved_settings().string_lossy_overrides(),
            skip_lint: args.skip_lint,
            coverage: args.coverage,
            coverage_min: args.coverage_min,
            analyze: args.analyze,
            baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
            changed_since: args.changed_since.clone(),
            precomputed_changed_files: args.precomputed_changed_files.clone(),
            json_summary: args.json_summary,
            ci_env: test_runner_ci_env(ci_job.as_ref()),
            passthrough_args: passthrough_args.clone(),
        },
        runner.run_dir(),
    );
    let workflow = runner.finish(
        observation,
        workflow,
        |observation, workflow| finish_test_observation(Some(observation), workflow),
        |observation, error| finish_test_observation_error(Some(observation), error),
    )?;

    Ok(report::from_main_workflow_with_ci_context(
        workflow,
        ci_profile::ci_context_for_job(ci_job.as_ref(), None),
    ))
}

fn ci_job_passthrough_args(job: Option<&CiResolvedJob>) -> Vec<String> {
    job.map(|job| job.spec.args.clone()).unwrap_or_default()
}

fn test_runner_ci_env(job: Option<&CiResolvedJob>) -> Vec<(String, String)> {
    let mut env = ci_profile::ci_job_env(job);

    for key in ["GITHUB_ACTIONS", "RELEASE_BLOCKING_COMMANDS"] {
        if let Ok(value) = std::env::var(key) {
            env.push((key.to_string(), value));
        }
    }

    env
}

struct TestObservation {
    active: ActiveObservation,
    run_dir: Option<PathBuf>,
}

fn start_test_observation(
    component_id: &str,
    source_path: &Path,
    args: &TestArgs,
    mode: &str,
    run_dir: Option<&RunDir>,
) -> Option<TestObservation> {
    let metadata = test_observation_initial_metadata(source_path, args, mode);
    ActiveObservation::start_best_effort(
        NewRunRecord::builder("test")
            .component_id(component_id)
            .command(test_observation_command(component_id, args))
            .cwd_path(source_path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(source_path))
            .metadata(metadata.clone())
            .build(),
    )
    .map(|active| TestObservation {
        active,
        run_dir: run_dir.map(|run_dir| run_dir.path().to_path_buf()),
    })
}

fn finish_test_observation(
    observation: Option<TestObservation>,
    workflow: &extension_test::TestRunWorkflowResult,
) {
    let Some(observation) = observation else {
        return;
    };

    let metadata = merge_metadata(
        merge_metadata(
            observation.active.initial_metadata().clone(),
            serde_json::json!({
            "observation_status": workflow.status,
            "exit_code": workflow.exit_code,
            "test_counts": workflow.test_counts,
            "failure_count": workflow.findings.as_ref().map(Vec::len).unwrap_or(0),
            "coverage": workflow.coverage,
            "baseline_regression": workflow.baseline_comparison.as_ref().map(|comparison| comparison.regression),
            "analysis_clusters": workflow.analysis.as_ref().map(|analysis| analysis.clusters.len()).unwrap_or(0),
            "test_scope": workflow.test_scope,
            "summary": workflow.summary,
            }),
        ),
        validation_progress_metadata_from_observation(&observation),
    );
    let status = if workflow.exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    persist_test_findings(&observation, workflow);
    observation.active.finish(status, Some(metadata));
}

fn persist_test_findings(
    observation: &TestObservation,
    workflow: &extension_test::TestRunWorkflowResult,
) {
    let mut records = Vec::new();
    if let Some(input) = &workflow.failure_analysis_input {
        records.extend(finding_records_from_test_analysis_input(
            observation.active.run_id(),
            input,
        ));
    }
    if let Some(analysis) = &workflow.analysis {
        records.extend(finding_records_from_failure_clusters(
            observation.active.run_id(),
            &analysis.clusters,
        ));
    }
    observation.active.record_findings(&records);
}

fn finish_test_drift_observation(
    observation: Option<TestObservation>,
    workflow: &extension_test::DriftWorkflowResult,
) {
    let Some(observation) = observation else {
        return;
    };

    let metadata = merge_metadata(
        observation.active.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": if workflow.exit_code == 0 { "pass" } else { "fail" },
            "exit_code": workflow.exit_code,
            "drift": workflow.report,
        }),
    );
    let status = if workflow.exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    observation.active.finish(status, Some(metadata));
}

fn finish_test_observation_error(
    observation: Option<TestObservation>,
    error: &homeboy::core::Error,
) {
    let Some(observation) = observation else {
        return;
    };

    let metadata = merge_metadata(
        merge_metadata(
            observation.active.initial_metadata().clone(),
            serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
            }),
        ),
        validation_progress_metadata_from_observation(&observation),
    );
    observation.active.finish_error(Some(metadata));
}

fn validation_progress_metadata_from_observation(
    observation: &TestObservation,
) -> serde_json::Value {
    observation
        .run_dir
        .as_ref()
        .and_then(|path| RunDir::from_existing(path.clone()).ok())
        .map(|run_dir| validation_progress_metadata(&run_dir))
        .unwrap_or_else(|| serde_json::json!({}))
}

fn test_observation_command(component_id: &str, args: &TestArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "test".to_string(),
        component_id.to_string(),
    ];
    if args.skip_lint {
        parts.push("--skip-lint".to_string());
    }
    if args.coverage {
        parts.push("--coverage".to_string());
    }
    if let Some(coverage_min) = args.coverage_min {
        parts.push(format!("--coverage-min={coverage_min}"));
    }
    if args.analyze {
        parts.push("--analyze".to_string());
    }
    if args.drift {
        parts.push("--drift".to_string());
    }
    if let Some(changed_since) = &args.changed_since {
        parts.push(format!("--changed-since={changed_since}"));
    }
    if args.json_summary {
        parts.push("--json-summary".to_string());
    }
    let passthrough_args = filter_homeboy_flags(&args.args);
    if !passthrough_args.is_empty() {
        parts.push("--".to_string());
        parts.extend(passthrough_args);
    }
    parts.join(" ")
}

fn test_observation_initial_metadata(
    source_path: &Path,
    args: &TestArgs,
    mode: &str,
) -> serde_json::Value {
    serde_json::json!({
        "source_path": source_path.to_string_lossy(),
        "mode": mode,
        "skip_lint": args.skip_lint,
        "coverage": args.coverage,
        "coverage_min": args.coverage_min,
        "analyze": args.analyze,
        "drift": args.drift,
        "baseline": {
            "baseline": args.baseline_args.baseline,
            "ignore_baseline": args.baseline_args.ignore_baseline,
            "ratchet": args.baseline_args.ratchet,
        },
        "changed_since": args.changed_since,
        "since": args.since,
        "json_summary": args.json_summary,
        "passthrough_args": filter_homeboy_flags(&args.args),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use clap::Parser;
    use homeboy::core::component::Component;
    use homeboy::core::extension::test::{TestAnalysisInput, TestFailure};
    use homeboy::core::observation::{FindingListFilter, ObservationStore};
    use homeboy::core::refactor::plan::{build_test_refactor_request, TestSourceOptions};
    use std::fs;
    use std::path::PathBuf;

    struct XdgGuard {
        prior: Option<String>,
    }

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self { prior }
        }

        fn set(value: &std::path::Path) -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::set_var("XDG_DATA_HOME", value);
            Self { prior }
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        test: TestArgs,
    }

    fn sample_args() -> TestArgs {
        TestCli::try_parse_from([
            "test",
            "homeboy",
            "--skip-lint",
            "--json-summary",
            "--changed-since",
            "origin/main",
            "--",
            "--filter=SmokeTest",
        ])
        .expect("parse sample args")
        .test
    }

    #[test]
    fn parses_ci_job_flag() {
        let cli = TestCli::try_parse_from(["test", "homeboy", "--ci-job", "unit"])
            .expect("test should parse --ci-job");

        assert_eq!(cli.test.ci_job.as_deref(), Some("unit"));
    }

    #[test]
    fn test_observation_start_persists_run_record() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();

            let observation = start_test_observation("homeboy", home.path(), &args, "test", None)
                .expect("observation should start");
            let run_id = observation.active.run_id().to_string();

            finish_test_observation_error(
                Some(observation),
                &homeboy::core::Error::validation_invalid_argument(
                    "fixture",
                    "simulated test error",
                    None,
                    None,
                ),
            );

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");

            assert_eq!(run.kind, "test");
            assert_eq!(run.status, "error");
            assert_eq!(run.component_id.as_deref(), Some("homeboy"));
            assert_eq!(run.metadata_json["changed_since"], "origin/main");
            assert_eq!(
                run.metadata_json["passthrough_args"][0],
                "--filter=SmokeTest"
            );
            assert_eq!(run.metadata_json["observation_status"], "error");
            assert!(
                run.metadata_json.get("run_dir").is_none(),
                "temporary run_dir paths must not be persisted in observation metadata"
            );
        });
    }

    #[test]
    fn test_observation_keeps_run_dir_out_of_initial_metadata() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let run_dir = RunDir::create().expect("run dir");

            let observation =
                start_test_observation("homeboy", home.path(), &args, "test", Some(&run_dir))
                    .expect("observation should start");
            let run_id = observation.active.run_id().to_string();

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");

            assert!(run.metadata_json.get("run_dir").is_none());
        });
    }

    #[test]
    fn test_observation_persists_test_failures_and_analysis_clusters() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let observation = start_test_observation("homeboy", home.path(), &args, "test", None)
                .expect("observation should start");
            let run_id = observation.active.run_id().to_string();
            let input = TestAnalysisInput {
                failures: vec![TestFailure {
                    test_name: "tests::fails".to_string(),
                    test_file: "tests/fails.rs".to_string(),
                    error_type: "AssertionFailed".to_string(),
                    message: "expected true".to_string(),
                    source_file: "src/lib.rs".to_string(),
                    source_line: 42,
                }],
                total: 2,
                passed: 1,
            };
            let analysis = extension_test::analyze::analyze("homeboy", &input);
            let cluster_fingerprint = format!("test-cluster::{}", analysis.clusters[0].id);

            finish_test_observation(
                Some(observation),
                &extension_test::TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: "homeboy".to_string(),
                    exit_code: 1,
                    test_counts: None,
                    findings: None,
                    failure_analysis_input: Some(input),
                    coverage: None,
                    baseline_comparison: None,
                    analysis: Some(analysis),
                    autofix: None,
                    hints: None,
                    test_scope: None,
                    summary: None,
                    raw_output: None,
                    extension_phase_timings: Vec::new(),
                },
            );

            let store = ObservationStore::open_initialized().expect("store");
            let findings = store
                .list_findings(FindingListFilter {
                    run_id: Some(run_id.clone()),
                    tool: Some("test".to_string()),
                    ..FindingListFilter::default()
                })
                .expect("list test findings");
            assert_eq!(findings.len(), 2);
            assert_eq!(findings[0].metadata_json["record_kind"], "failure");
            assert_eq!(findings[0].file.as_deref(), Some("tests/fails.rs"));
            assert_eq!(findings[0].line, Some(42));
            assert_eq!(findings[1].metadata_json["record_kind"], "analysis_cluster");
            assert_eq!(findings[1].metadata_json["count"], 1);

            let cluster = store
                .list_findings(FindingListFilter {
                    run_id: Some(run_id),
                    tool: Some("test".to_string()),
                    fingerprint: Some(cluster_fingerprint),
                    ..FindingListFilter::default()
                })
                .expect("list cluster by fingerprint");
            assert_eq!(cluster.len(), 1);
            assert_eq!(cluster[0].metadata_json["record_kind"], "analysis_cluster");
        });
    }

    #[test]
    fn test_observation_start_is_best_effort_when_store_unavailable() {
        with_isolated_home(|home| {
            let bad_data_home = home.path().join("not-a-dir");
            fs::write(&bad_data_home, "file blocks observation dir").expect("write marker");
            let _xdg = XdgGuard::set(&bad_data_home);

            let observation =
                start_test_observation("homeboy", home.path(), &sample_args(), "test", None);

            assert!(observation.is_none());
        });
    }

    #[test]
    fn parses_one_shot_extension_override() {
        let cli = TestCli::try_parse_from([
            "test",
            "--path",
            "/tmp/repo",
            "--extension",
            "fixture-test",
            "--changed-since",
            "origin/main",
        ])
        .expect("test should parse --extension override");

        assert_eq!(cli.test.extension_override.extensions, vec!["fixture-test"]);
        assert_eq!(cli.test.changed_since.as_deref(), Some("origin/main"));
    }

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

        let args = vec![
            "--extension".to_string(),
            "fixture-test".to_string(),
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

        let request = build_test_refactor_request(
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
