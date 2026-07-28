use clap::Args;

use homeboy::core::ci_profile::{self, CiResolvedJob};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    finding_records_from_failure_clusters, finding_records_from_test_analysis_input,
    merge_metadata, ActiveObservation, NewRunRecord, RunStatus,
};
use homeboy_extension::test as extension_test;
use homeboy_extension::test::{
    build_test_summary, detect_test_drift, parse_test_failures_from_text,
    parse_test_results_failures_file, parse_test_results_file, parse_test_results_text, report,
    run_self_check_test_workflow_with_progress, test_failure_summary_items, TestAnalysisInput,
    TestCommandOutput, TestFailure, TestRunWorkflowArgs,
};
use homeboy_extension::ExtensionCapability;
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use super::source_command::{resolve_ci_job_for_command, resolve_source_context};
use super::utils::args::{
    filter_passthrough_args, BaselineArgs, ExtensionOverrideArgs, PassthroughCommand,
    PositionalComponentArgs, SettingArgs,
};
use super::utils::response::actionable_metadata_value_for_run_ref;
use super::CmdResult;
use crate::command_contract::{LabCommandContract, TEST_LAB_LABEL};
use crate::core::observation::ObservedWorkflowRunner;
use homeboy::core::validation_progress::validation_progress_metadata;

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

    #[arg(long, hide = true, value_name = "JSON")]
    pub lab_changed_files_json: Option<String>,

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

    #[arg(skip)]
    pub restore_checkout: bool,
}

impl TestArgs {
    pub(crate) fn lab_contract(&self) -> LabCommandContract {
        if self.baseline_args.baseline || self.baseline_args.ratchet {
            return LabCommandContract::local_only(
                TEST_LAB_LABEL,
                "test baseline and ratchet modes write source-owned baseline state on the controller",
            );
        }
        LabCommandContract::portable(TEST_LAB_LABEL, self.write.then_some("--write"), true, &[])
            .release_gate()
    }

    pub(crate) fn should_use_self_check_dispatch(&self, cli_passthrough_args: &[String]) -> bool {
        !self.skip_lint
            && !self.coverage
            && self.coverage_min.is_none()
            && !self.analyze
            && !self.drift
            && !self.write
            && self.changed_since.is_none()
            && self.precomputed_changed_files.is_none()
            && self.lab_changed_files_json.is_none()
            && self.ci_job.is_none()
            && cli_passthrough_args.is_empty()
            && !self.setting_args.has_overrides()
            && !self.baseline_args.baseline
            && !self.baseline_args.ignore_baseline
            && !self.baseline_args.ratchet
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

pub fn run(args: TestArgs) -> CmdResult<TestCommandOutput> {
    let source_ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        None,
    )?;
    let cli_passthrough_args = filter_homeboy_flags(&args.args);

    if args.should_use_self_check_dispatch(&cli_passthrough_args)
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
        let run_id = observation
            .as_ref()
            .map(|observation| observation.run_id().to_string());
        if let Some(run_id) = run_id.as_deref() {
            runner.bind_run_id(run_id)?;
        }
        let workflow = run_self_check_test_workflow_with_progress(
            &source_ctx.component,
            &source_ctx.source_path,
            source_ctx.component_id.clone(),
            args.json_summary,
            Some(runner.run_dir()),
            observation.as_ref().map(|observation| &observation.active),
        );

        let scratch_succeeded = workflow
            .as_ref()
            .is_ok_and(|workflow| workflow.exit_code == 0);
        let workflow = runner.finish_with_scratch_outcome(
            observation,
            workflow,
            scratch_succeeded,
            |observation, workflow| finish_test_observation(Some(observation), workflow),
            |observation, error| finish_test_observation_error(Some(observation), error),
        )?;

        let (mut output, exit_code) = report::from_main_workflow(workflow);
        attach_test_actionable(&mut output, run_id);
        return Ok((output, exit_code));
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
        let run_id = observation
            .as_ref()
            .map(|observation| observation.run_id().to_string());
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
        let (mut output, exit_code) = report::from_drift_workflow(result);
        attach_test_actionable(&mut output, run_id);
        return Ok((output, exit_code));
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
    let run_id = observation
        .as_ref()
        .map(|observation| observation.run_id().to_string());
    if let Some(run_id) = run_id.as_deref() {
        runner.bind_run_id(run_id)?;
    }
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
            settings_json: ctx.resolved_settings().json_overrides(),
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
            precomputed_changed_files: changed_files_from_args(&args)?,
            json_summary: args.json_summary,
            restore_checkout: args.restore_checkout,
            ci_env: test_runner_ci_env(ci_job.as_ref())
                .into_iter()
                .chain(extension_test::portable_env(&ctx.component)?.public_env)
                .collect(),
            passthrough_args: passthrough_args.clone(),
        },
        runner.run_dir(),
    );
    let mut workflow = workflow;
    let mut collection_failed = false;
    if let (Some(observation), Ok(workflow_result)) = (observation.as_ref(), workflow.as_mut()) {
        if let Err(error) = persist_declared_test_artifacts(observation, workflow_result) {
            collection_failed = true;
            workflow = Err(homeboy::core::Error::internal_unexpected(format!(
                "test artifact collection failure: {error}"
            )));
        }
    }
    let scratch_succeeded = collection_failed
        || workflow
            .as_ref()
            .is_ok_and(|workflow| workflow.exit_code == 0);
    let workflow = if collection_failed {
        runner.finish_with_finalized_error_cleanup(
            observation,
            workflow,
            scratch_succeeded,
            |observation, workflow| finish_test_observation(Some(observation), workflow),
            |observation, error| finish_test_observation_error(Some(observation), error),
        )
    } else {
        runner.finish_with_scratch_outcome(
            observation,
            workflow,
            scratch_succeeded,
            |observation, workflow| finish_test_observation(Some(observation), workflow),
            |observation, error| finish_test_observation_error(Some(observation), error),
        )
    }?;

    let (mut output, exit_code) = report::from_main_workflow_with_ci_context(
        workflow,
        ci_profile::ci_context_for_job(ci_job.as_ref(), None),
    );
    attach_test_actionable(&mut output, run_id);
    Ok((output, exit_code))
}

fn attach_test_actionable(output: &mut TestCommandOutput, run_id: Option<String>) {
    if let Some(run_id) = run_id {
        output.actionable = Some(actionable_metadata_value_for_run_ref(
            run_id,
            "test",
            "homeboy-test",
        ));
    }
}

fn changed_files_from_args(args: &TestArgs) -> homeboy::core::Result<Option<Vec<String>>> {
    if args.precomputed_changed_files.is_some() {
        return Ok(args.precomputed_changed_files.clone());
    }
    args.lab_changed_files_json
        .as_deref()
        .map(parse_lab_changed_files_json)
        .transpose()
}

fn parse_lab_changed_files_json(raw: &str) -> homeboy::core::Result<Vec<String>> {
    serde_json::from_str(raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "lab_changed_files_json",
            format!("invalid Lab changed-file payload: {error}"),
            None,
            None,
        )
    })
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

impl TestObservation {
    fn run_id(&self) -> &str {
        self.active.run_id()
    }
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

    let child_supervision = child_supervision_metadata_from_observation(&observation);
    let interrupted = child_supervision["child_supervision"]["status"] == "interrupted";
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
        merge_metadata(
            validation_progress_metadata_from_observation(&observation),
            child_supervision,
        ),
    );
    let metadata = if interrupted {
        merge_metadata(
            metadata,
            serde_json::json!({ "observation_status": "interrupted" }),
        )
    } else {
        metadata
    };
    let status = if interrupted {
        RunStatus::Error
    } else if workflow.exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    persist_test_findings(&observation, workflow);
    persist_validation_progress_artifacts(&observation);
    persist_child_supervision_artifact(&observation);
    observation.active.finish(status, Some(metadata));
}

/// Persist provider-declared test artifacts before the runner connection or
/// scratch run directory disappears. The declaration shape stays opaque: this
/// boundary only recognizes a generic `artifact://files/<relative path>`
/// locator rooted in the invocation run directory.
fn persist_declared_test_artifacts(
    observation: &TestObservation,
    workflow: &mut extension_test::TestRunWorkflowResult,
) -> homeboy::core::Result<()> {
    let Some(run_dir) = observation
        .run_dir
        .as_ref()
        .and_then(|path| RunDir::from_existing(path.clone()).ok())
    else {
        return Ok(());
    };

    let mut reported_locator_replacements = Vec::new();
    for (timing_index, timing) in workflow.extension_phase_timings.iter_mut().enumerate() {
        for (artifact_index, declaration) in timing.artifacts.iter_mut().enumerate() {
            let Some(locator) = artifact_locator(declaration).map(str::to_string) else {
                continue;
            };
            let Some(relative_path) = artifact_locator_relative_path(&locator) else {
                let record = record_unavailable_test_artifact(
                    observation,
                    timing_index,
                    artifact_index,
                    &timing.name,
                    &locator,
                    "the locator has no controller-local file provenance",
                )?;
                reported_locator_replacements.push(reported_test_artifact_locator_replacement(
                    &record, &locator,
                ));
                *declaration = persisted_test_artifact_declaration(&record, &timing.name);
                continue;
            };
            let mut source =
                match open_test_artifact_no_follow(&run_dir.path().join("files"), &relative_path) {
                    Ok(source) => source,
                    Err(error) => {
                        let record = record_unavailable_test_artifact(
                    observation,
                    timing_index,
                    artifact_index,
                    &timing.name,
                    &locator,
                    &format!("the declared controller-local artifact file is unavailable: {error}"),
                )?;
                        reported_locator_replacements.push(
                            reported_test_artifact_locator_replacement(&record, &locator),
                        );
                        *declaration = persisted_test_artifact_declaration(&record, &timing.name);
                        continue;
                    }
                };
            let record = observation
                .active
                .store()
                .record_artifact_from_open_file_with_metadata(
                    observation.active.run_id(),
                    "test_artifact",
                    &mut source,
                    serde_json::json!({
                        "source": "extension_phase_timing",
                        "phase": timing.name,
                        "locator": &locator,
                    }),
                )?;
            reported_locator_replacements.push(reported_test_artifact_locator_replacement(
                &record, &locator,
            ));
            *declaration = persisted_test_artifact_declaration(&record, &timing.name);
        }
    }
    enrich_test_result_from_persisted_artifacts(observation, workflow)?;
    rewrite_reported_artifact_locators(workflow, &reported_locator_replacements);
    Ok(())
}

fn rewrite_reported_artifact_locators(
    workflow: &mut extension_test::TestRunWorkflowResult,
    replacements: &[(String, String)],
) {
    let Some(raw_output) = workflow.raw_output.as_mut() else {
        return;
    };
    for (locator, replacement) in replacements {
        raw_output.stdout_tail = raw_output.stdout_tail.replace(locator, replacement);
        raw_output.stderr_tail = raw_output.stderr_tail.replace(locator, replacement);
    }
}

fn open_test_artifact_no_follow(
    files_root: &Path,
    relative_path: &Path,
) -> homeboy::core::Result<std::fs::File> {
    #[cfg(windows)]
    {
        return open_test_artifact_no_follow_windows(files_root, relative_path);
    }

    #[cfg(unix)]
    {
        return open_test_artifact_no_follow_unix(files_root, relative_path);
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (files_root, relative_path);
        return Err(homeboy::core::Error::internal_unexpected(
            "descriptor-relative test artifact ingestion requires Unix openat support",
        ));
    }
}

#[cfg(windows)]
fn open_test_artifact_no_follow_windows(
    files_root: &Path,
    relative_path: &Path,
) -> homeboy::core::Result<std::fs::File> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let mut current = files_root.to_path_buf();
    let components = relative_path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "artifact",
                "test artifact path must contain normal relative components",
                Some(relative_path.display().to_string()),
                None,
            ));
        };
        current.push(name);
        let final_component = index + 1 == components.len();
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if final_component {
                    0
                } else {
                    FILE_FLAG_BACKUP_SEMANTICS
                },
        );
        let file = options.open(&current).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "open test artifact component {}",
                    current.display()
                )),
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "inspect test artifact component {}",
                    current.display()
                )),
            )
        })?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (!final_component && !metadata.is_dir())
            || (final_component && !metadata.is_file())
        {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "artifact",
                "test artifact path contains a reparse point or non-regular component",
                Some(relative_path.display().to_string()),
                None,
            ));
        }
        if final_component {
            return Ok(file);
        }
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "artifact",
        "test artifact path is empty",
        Some(relative_path.display().to_string()),
        None,
    ))
}

#[cfg(unix)]
fn open_test_artifact_no_follow_unix(
    files_root: &Path,
    relative_path: &Path,
) -> homeboy::core::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let mut directory = options.open(files_root).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(format!("open test artifact root {}", files_root.display())),
        )
    })?;
    let components = relative_path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "artifact",
                "test artifact path must contain normal relative components",
                Some(relative_path.display().to_string()),
                None,
            ));
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact",
                "test artifact path contains an embedded NUL byte",
                Some(relative_path.display().to_string()),
                None,
            )
        })?;
        let final_component = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_component {
                0
            } else {
                libc::O_DIRECTORY
            };
        // Each artifact-controlled component is resolved from its opened parent.
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(homeboy::core::Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(format!(
                    "open test artifact component {}",
                    relative_path.display()
                )),
            ));
        }
        let opened = unsafe { std::fs::File::from_raw_fd(descriptor) };
        directory = opened;
    }
    let file = directory;
    let metadata = file.metadata().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("inspect opened test artifact".to_string()),
        )
    })?;
    if !metadata.is_file() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "artifact",
            "opened test artifact is not a regular file",
            Some(relative_path.display().to_string()),
            None,
        ));
    }
    Ok(file)
}

fn enrich_test_result_from_persisted_artifacts(
    observation: &TestObservation,
    workflow: &mut extension_test::TestRunWorkflowResult,
) -> homeboy::core::Result<()> {
    let artifacts = observation
        .active
        .store()
        .list_artifacts(observation.active.run_id())?;
    let mut parsed_failures = None;
    let mut persisted_refs = Vec::new();
    for artifact in artifacts {
        let locator = artifact
            .metadata_json
            .get("locator")
            .and_then(Value::as_str);
        let Some(locator) = locator else { continue };
        if artifact.artifact_type != "file" {
            continue;
        }
        persisted_refs.push((
            locator.to_string(),
            format!(
                "homeboy://run/{}/artifact/{}",
                observation.active.run_id(),
                artifact.id
            ),
        ));
        if locator.ends_with("test-results.json") && workflow.test_counts.is_none() {
            workflow.test_counts = parse_test_results_file(Path::new(&artifact.path))?;
        }
        if locator.ends_with("test-results.json") && parsed_failures.is_none() {
            parsed_failures = parse_test_results_failures_file(
                Path::new(&artifact.path),
                workflow.test_counts.as_ref(),
            )?;
        }
        if locator.ends_with("phpunit-output.log") {
            let output = std::fs::read_to_string(&artifact.path).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(format!("read persisted test artifact {}", artifact.id)),
                )
            })?;
            if workflow.test_counts.is_none() {
                workflow.test_counts = parse_test_results_text(&output);
            }
            if parsed_failures.is_none() {
                parsed_failures =
                    parse_test_failures_from_text(&output, workflow.test_counts.as_ref());
            }
        }
    }
    if let Some(raw_output) = workflow.raw_output.as_mut() {
        for (locator, reference) in persisted_refs {
            raw_output.stdout_tail = raw_output.stdout_tail.replace(&locator, &reference);
            raw_output.stderr_tail = raw_output.stderr_tail.replace(&locator, &reference);
        }
    }
    if workflow.failure_analysis_input.is_none() {
        workflow.failure_analysis_input = parsed_failures;
    }
    if workflow.failure_analysis_input.is_none()
        && workflow
            .test_counts
            .as_ref()
            .is_some_and(|counts| counts.failed > 0)
    {
        let counts = workflow.test_counts.as_ref().expect("checked above");
        let input = TestAnalysisInput {
            failures: vec![TestFailure {
                test_name: "provider test results".to_string(),
                test_file: String::new(),
                error_type: "test_failure".to_string(),
                message: format!(
                    "{} test failure(s) reported by persisted test results",
                    counts.failed
                ),
                source_file: String::new(),
                source_line: 0,
            }],
            total: counts.total,
            passed: counts.passed,
        };
        workflow.failure_analysis_input = Some(input);
    }
    if workflow.findings.is_none() {
        workflow.findings = workflow
            .failure_analysis_input
            .as_ref()
            .and_then(homeboy::core::observation::homeboy_findings_from_test_analysis_input);
    }
    let mut summary = build_test_summary(
        workflow.test_counts.as_ref(),
        workflow.analysis.as_ref(),
        workflow.exit_code,
    );
    if let Some(input) = &workflow.failure_analysis_input {
        summary.failures = test_failure_summary_items(&input.failures);
    }
    workflow.summary = Some(summary);
    Ok(())
}

fn record_unavailable_test_artifact(
    observation: &TestObservation,
    timing_index: usize,
    artifact_index: usize,
    phase: &str,
    locator: &str,
    reason: &str,
) -> homeboy::core::Result<homeboy::core::observation::ArtifactRecord> {
    let artifact = homeboy::core::observation::ArtifactRecord {
        id: format!(
            "{}-test-artifact-{timing_index}-{artifact_index}",
            observation.active.run_id()
        ),
        run_id: observation.active.run_id().to_string(),
        kind: "test_artifact".to_string(),
        artifact_type: "metadata-only".to_string(),
        path: format!("metadata-only:{locator}"),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: None,
        mime: None,
        metadata_json: serde_json::json!({
            "source": "extension_phase_timing",
            "phase": phase,
            "locator": locator,
            "unavailable_reason": reason,
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    observation.active.store().import_artifact(&artifact)?;
    Ok(artifact)
}

fn persisted_test_artifact_declaration(
    artifact: &homeboy::core::observation::ArtifactRecord,
    phase: &str,
) -> Value {
    serde_json::json!({
        "schema": "homeboy/review-artifact/v1",
        "run_id": artifact.run_id,
        "artifact_id": artifact.id,
        "ref": format!("homeboy://run/{}/artifact/{}", artifact.run_id, artifact.id),
        "phase": phase,
        "available": artifact.artifact_type == "file",
        "unavailable_reason": artifact.metadata_json.get("unavailable_reason"),
    })
}

fn reported_test_artifact_locator_replacement(
    artifact: &homeboy::core::observation::ArtifactRecord,
    locator: &str,
) -> (String, String) {
    let reference = format!("homeboy://run/{}/artifact/{}", artifact.run_id, artifact.id);
    let replacement = if artifact.artifact_type == "file" {
        reference
    } else {
        let reason = artifact
            .metadata_json
            .get("unavailable_reason")
            .and_then(Value::as_str)
            .unwrap_or("evidence collection unavailable");
        format!("{reference} (evidence collection unavailable: {reason})")
    };
    (locator.to_string(), replacement)
}

fn artifact_locator(declaration: &Value) -> Option<&str> {
    ["path", "url", "ref", "uri"]
        .into_iter()
        .find_map(|key| declaration.get(key).and_then(Value::as_str))
        .filter(|locator| !locator.trim().is_empty())
}

fn artifact_locator_relative_path(locator: &str) -> Option<PathBuf> {
    let relative = locator.strip_prefix("artifact://files/")?;
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return None;
    }
    let components = relative.components().collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let normalized = components.iter().collect::<PathBuf>();
    (normalized.as_os_str() == relative.as_os_str()).then_some(normalized)
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

fn persist_validation_progress_artifacts(observation: &TestObservation) {
    let Some(run_dir) = observation
        .run_dir
        .as_ref()
        .and_then(|path| RunDir::from_existing(path.clone()).ok())
    else {
        return;
    };
    let Some(ledger) =
        homeboy::core::validation_progress::ValidationProgressLedger::read_from_run_dir(&run_dir)
    else {
        return;
    };

    for command in ledger.commands {
        for (stream, artifact) in [
            ("stdout", command.stdout_artifact),
            ("stderr", command.stderr_artifact),
        ] {
            let Some(artifact) = artifact else {
                continue;
            };
            observation.active.record_artifact_if_file(
                &format!("validation_command_{}_{}", command.index + 1, stream),
                &run_dir.path().join(artifact),
            );
        }
    }
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
        merge_metadata(
            validation_progress_metadata_from_observation(&observation),
            child_supervision_metadata_from_observation(&observation),
        ),
    );
    persist_child_supervision_artifact(&observation);
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

fn child_supervision_metadata_from_observation(observation: &TestObservation) -> serde_json::Value {
    observation
        .run_dir
        .as_ref()
        .and_then(|path| RunDir::from_existing(path.clone()).ok())
        .and_then(|run_dir| {
            std::fs::read_to_string(
                run_dir.step_file(homeboy::core::engine::run_dir::files::CHILD_SUPERVISION),
            )
            .ok()
        })
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .map(|supervision| serde_json::json!({ "child_supervision": supervision }))
        .unwrap_or_else(|| serde_json::json!({}))
}

fn persist_child_supervision_artifact(observation: &TestObservation) {
    let Some(run_dir) = observation
        .run_dir
        .as_ref()
        .and_then(|path| RunDir::from_existing(path.clone()).ok())
    else {
        return;
    };
    observation.active.record_artifact_if_file(
        "child_supervision",
        &run_dir.step_file(homeboy::core::engine::run_dir::files::CHILD_SUPERVISION),
    );
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
    use homeboy::core::observation::{FindingListFilter, ObservationStore};
    use homeboy::refactor::plan::{build_test_refactor_request, TestSourceOptions};
    use homeboy_extension::test::{TestAnalysisInput, TestCounts, TestFailure};
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
    fn parses_settings_json_file_and_profile_alias() {
        let cli = TestCli::try_parse_from([
            "test",
            "homeboy",
            "--settings-json-file",
            "base.json",
            "--settings-profile",
            "profile.json",
        ])
        .expect("test should parse settings file flags");

        assert_eq!(
            cli.test.setting_args.settings_json_file,
            vec![PathBuf::from("base.json"), PathBuf::from("profile.json")]
        );
    }

    #[test]
    fn baseline_and_ratchet_test_modes_remain_controller_local() {
        for flag in ["--baseline", "--ratchet"] {
            let args = TestCli::try_parse_from(["test", "homeboy", flag])
                .expect("test mode should parse")
                .test;
            let contract = args.lab_contract();

            assert!(matches!(
                contract.portability,
                crate::command_contract::LabCommandPortability::LocalOnly(reason)
                    if reason.contains("source-owned baseline state")
            ));
            assert!(!contract.routing_policy.default_lab_offload);
        }
    }

    #[test]
    fn externally_configured_test_mode_remains_lab_portable() {
        let args = TestCli::try_parse_from([
            "test",
            "homeboy",
            "--settings-json-file",
            "phpunit-db-service.json",
            "--setting-json",
            "database_service={\"host\":\"127.0.0.1\",\"port\":3306}",
        ])
        .expect("external database-service settings should parse")
        .test;

        assert!(args.lab_contract().is_portable());
    }

    #[test]
    fn self_check_dispatch_only_allows_unscoped_default_test() {
        let default = TestCli::try_parse_from(["test", "homeboy"])
            .expect("default test should parse")
            .test;
        assert!(default.should_use_self_check_dispatch(&filter_homeboy_flags(&default.args)));

        for argv in [
            vec!["test", "homeboy", "--skip-lint"],
            vec!["test", "homeboy", "--coverage"],
            vec!["test", "homeboy", "--coverage-min", "80"],
            vec!["test", "homeboy", "--analyze"],
            vec!["test", "homeboy", "--changed-since", "origin/main"],
            vec!["test", "homeboy", "--setting", "runner=ci"],
            vec!["test", "homeboy", "--settings-json-file", "profile.json"],
            vec!["test", "homeboy", "--baseline"],
            vec!["test", "homeboy", "--", "--filter=SmokeTest"],
        ] {
            let args = TestCli::try_parse_from(argv.clone())
                .unwrap_or_else(|error| panic!("test args should parse for {argv:?}: {error}"))
                .test;
            assert!(
                !args.should_use_self_check_dispatch(&filter_homeboy_flags(&args.args)),
                "scoped or behavior-changing args must use main test workflow: {argv:?}"
            );
        }
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
    fn injected_artifact_store_failure_terminalizes_collection_and_cleans_scratch() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let runner = ObservedWorkflowRunner::create("test homeboy").expect("runner");
            let scratch_path = runner.run_dir().path().to_path_buf();
            let files = scratch_path.join("files");
            fs::create_dir(&files).expect("artifact files dir");
            fs::write(files.join("test-results.json"), b"{\"failed\":1}").expect("result bytes");
            let observation = start_test_observation(
                "homeboy",
                home.path(),
                &args,
                "test",
                Some(runner.run_dir()),
            )
            .expect("observation");
            let run_id = observation.active.run_id().to_string();
            runner.bind_run_id(&run_id).expect("bind run id");
            let database = homeboy::core::observation::store::database_path()
                .expect("observation database path");
            rusqlite::Connection::open(database)
                .expect("open observation database")
                .execute_batch(
                    "CREATE TRIGGER fail_test_artifact_insert BEFORE INSERT ON artifacts WHEN NEW.kind = 'test_artifact' BEGIN SELECT RAISE(ABORT, 'injected test artifact store failure'); END;",
                )
                .expect("install artifact insertion fault");
            let mut workflow = extension_test::TestRunWorkflowResult {
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 1,
                test_counts: None,
                test_durations: None,
                findings: None,
                failure_analysis_input: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints: None,
                test_scope: None,
                summary: None,
                raw_output: None,
                extension_phase_timings: vec![homeboy_extension::ExtensionPhaseTiming {
                    name: "provider-test".to_string(),
                    duration_ms: 1,
                    status: Some("failed".to_string()),
                    message: None,
                    artifacts: vec![serde_json::json!({
                        "ref": "artifact://files/test-results.json"
                    })],
                    metadata: Default::default(),
                }],
            };
            let collection_error = persist_declared_test_artifacts(&observation, &mut workflow)
                .expect_err("injected artifact store failure must propagate");
            assert!(collection_error
                .to_string()
                .contains("injected test artifact store failure"));
            let error = homeboy::core::Error::internal_unexpected(format!(
                "test artifact collection failure: {collection_error}"
            ));

            let result = runner.finish_with_finalized_error_cleanup(
                Some(observation),
                Err::<extension_test::TestRunWorkflowResult, _>(error),
                true,
                |observation, workflow| finish_test_observation(Some(observation), workflow),
                |observation, error| finish_test_observation_error(Some(observation), error),
            );

            let error = result.expect_err("collection failure must be terminal");
            assert!(error
                .to_string()
                .contains("test artifact collection failure"));
            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");
            assert_eq!(run.status, "error");
            assert_eq!(run.metadata_json["observation_status"], "error");
            assert!(run.metadata_json["error"]
                .as_str()
                .expect("error metadata")
                .contains("injected test artifact store failure"));
            assert!(
                !scratch_path.exists(),
                "terminal collection failures clean scratch after recording the error"
            );
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

            let workflow = extension_test::TestRunWorkflowResult {
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 1,
                test_counts: None,
                test_durations: None,
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
            };
            finish_test_observation(Some(observation), &workflow);

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");
            assert_eq!(run.status, "fail");
            assert_eq!(run.metadata_json["observation_status"], "failed");
            assert_eq!(run.metadata_json["exit_code"], 1);
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
    fn test_observation_attaches_validation_command_output() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let run_dir = RunDir::create().expect("run dir");
            let stdout = homeboy::core::validation_progress::write_command_artifact(
                &run_dir,
                0,
                "stdout",
                "test fixture::fails ... FAILED",
            )
            .expect("write stdout");
            let stderr = homeboy::core::validation_progress::write_command_artifact(
                &run_dir,
                0,
                "stderr",
                "compiler diagnostic",
            )
            .expect("write stderr");
            let mut progress = homeboy::core::validation_progress::ValidationProgressRecorder::new(
                &run_dir,
                None,
                vec![("test runner".to_string(), "fixture".to_string())],
            )
            .expect("progress");
            progress.start(0).expect("start");
            progress.finish(0, 101, stdout, stderr).expect("finish");

            let observation =
                start_test_observation("homeboy", home.path(), &args, "test", Some(&run_dir))
                    .expect("observation should start");
            let run_id = observation.active.run_id().to_string();
            finish_test_observation(
                Some(observation),
                &extension_test::TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: "homeboy".to_string(),
                    exit_code: 101,
                    test_counts: Some(TestCounts::new(0, 0, 0, 0)),
                    test_durations: None,
                    findings: None,
                    failure_analysis_input: None,
                    coverage: None,
                    baseline_comparison: None,
                    analysis: None,
                    autofix: None,
                    hints: None,
                    test_scope: None,
                    summary: None,
                    raw_output: None,
                    extension_phase_timings: Vec::new(),
                },
            );

            let artifacts = ObservationStore::open_initialized()
                .expect("store")
                .list_artifacts(&run_id)
                .expect("list artifacts");
            assert_eq!(artifacts.len(), 2);
            assert!(artifacts
                .iter()
                .any(|artifact| artifact.kind == "validation_command_1_stdout"));
            assert!(artifacts
                .iter()
                .any(|artifact| artifact.kind == "validation_command_1_stderr"));
            run_dir.cleanup();
        });
    }

    #[test]
    fn failing_test_persists_declared_artifacts_and_records_missing_provenance() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let run_dir = RunDir::create().expect("run dir");
            let files = run_dir.path().join("files");
            fs::create_dir(&files).expect("artifact files dir");
            fs::write(files.join("test-results.json"), b"{\"failed\":1}").expect("result bytes");
            fs::write(
                files.join("phpunit-output.log"),
                b"PHPUnit 10.0\n\nThere was 1 failure:\n\n1) Tests\\ExampleTest::test_it_fails\nFailed asserting that false is true.\n\n/workspace/tests/ExampleTest.php:42\n\nFAILURES!\nTests: 1, Assertions: 1, Failures: 1\n",
            )
            .expect("log bytes");
            let outside = home.path().join("outside.log");
            fs::write(&outside, b"outside bytes").expect("outside bytes");
            std::os::unix::fs::symlink(&outside, files.join("symlink.log"))
                .expect("symlink artifact");
            let outside_dir = home.path().join("outside-dir");
            fs::create_dir(&outside_dir).expect("outside directory");
            fs::write(outside_dir.join("escaped.log"), b"escaped bytes").expect("escaped bytes");
            std::os::unix::fs::symlink(&outside_dir, files.join("nested"))
                .expect("intermediate symlink artifact");
            let observation =
                start_test_observation("homeboy", home.path(), &args, "test", Some(&run_dir))
                    .expect("observation");
            let run_id = observation.active.run_id().to_string();
            let timing = homeboy_extension::ExtensionPhaseTiming {
                name: "provider-test".to_string(),
                duration_ms: 1,
                status: Some("failed".to_string()),
                message: None,
                artifacts: vec![
                    serde_json::json!({ "ref": "artifact://files/test-results.json" }),
                    serde_json::json!({ "ref": "artifact://files/phpunit-output.log" }),
                    serde_json::json!({ "ref": "artifact://files/missing.log" }),
                    serde_json::json!({ "ref": "artifact://files/../outside.log" }),
                    serde_json::json!({ "ref": "artifact://files/symlink.log" }),
                    serde_json::json!({ "ref": "artifact://files/nested/escaped.log" }),
                ],
                metadata: Default::default(),
            };
            let mut workflow = extension_test::TestRunWorkflowResult {
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 1,
                test_counts: None,
                test_durations: None,
                findings: None,
                failure_analysis_input: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints: None,
                test_scope: None,
                summary: None,
                raw_output: None,
                extension_phase_timings: vec![timing],
            };
            persist_declared_test_artifacts(&observation, &mut workflow)
                .expect("persist declared artifacts");
            assert_eq!(
                workflow.test_counts.as_ref().map(|counts| counts.failed),
                Some(1)
            );
            assert_eq!(
                workflow
                    .failure_analysis_input
                    .as_ref()
                    .map(|input| input.failures.len()),
                Some(1)
            );
            let failure = &workflow
                .failure_analysis_input
                .as_ref()
                .expect("parsed PHPUnit failure")
                .failures[0];
            assert_eq!(failure.test_name, "Tests\\ExampleTest::test_it_fails");
            assert_eq!(failure.message, "Failed asserting that false is true.");
            assert_eq!(failure.source_file, "/workspace/tests/ExampleTest.php");
            assert_eq!(failure.source_line, 42);
            assert!(workflow
                .findings
                .as_ref()
                .is_some_and(|findings| !findings.is_empty()));
            assert_eq!(
                workflow.findings.as_ref().expect("findings")[0].message,
                "phpunit_failure: Failed asserting that false is true."
            );
            assert_eq!(
                workflow.findings.as_ref().expect("findings")[0].metadata["test_name"],
                "Tests\\ExampleTest::test_it_fails"
            );
            assert_eq!(
                workflow.summary.as_ref().expect("summary").failures[0].test_name,
                "Tests\\ExampleTest::test_it_fails"
            );
            assert_eq!(
                workflow.summary.as_ref().expect("summary").failures[0].message,
                "Failed asserting that false is true."
            );
            assert_eq!(
                workflow.extension_phase_timings[0].artifacts[0]["ref"],
                format!(
                    "homeboy://run/{run_id}/artifact/{}",
                    workflow.extension_phase_timings[0].artifacts[0]["artifact_id"]
                        .as_str()
                        .expect("artifact id")
                )
            );
            assert!(workflow.extension_phase_timings[0].artifacts[0]
                .get("source_locator")
                .is_none());
            finish_test_observation(Some(observation), &workflow);

            run_dir.cleanup();
            let artifacts = ObservationStore::open_initialized()
                .expect("store")
                .list_artifacts(&run_id)
                .expect("artifacts");
            assert_eq!(artifacts.len(), 6);
            assert!(artifacts.iter().any(|artifact| {
                artifact.artifact_type == "file"
                    && fs::read(&artifact.path).ok().as_deref() == Some(b"{\"failed\":1}")
            }));
            assert!(artifacts.iter().any(|artifact| {
                artifact.artifact_type == "file"
                    && fs::read(&artifact.path)
                        .ok()
                        .is_some_and(|contents| contents.starts_with(b"PHPUnit 10.0"))
            }));
            let missing = artifacts
                .iter()
                .find(|artifact| artifact.artifact_type == "metadata-only")
                .expect("missing provenance record");
            assert_eq!(
                missing.metadata_json["locator"],
                "artifact://files/missing.log"
            );
            assert!(missing.metadata_json["unavailable_reason"]
                .as_str()
                .expect("reason")
                .contains("unavailable"));
            assert_eq!(
                artifacts
                    .iter()
                    .filter(|artifact| artifact.artifact_type == "metadata-only")
                    .count(),
                4
            );
            assert!(artifacts.iter().any(|artifact| {
                artifact.metadata_json["locator"] == "artifact://files/../outside.log"
            }));
            assert!(artifacts.iter().any(|artifact| {
                artifact.metadata_json["locator"] == "artifact://files/symlink.log"
            }));
            assert!(artifacts.iter().any(|artifact| {
                artifact.metadata_json["locator"] == "artifact://files/nested/escaped.log"
            }));
        });
    }

    #[test]
    fn test_artifact_locator_requires_a_normal_relative_path() {
        for locator in [
            "artifact://files/../escape.log",
            "artifact://files/./result.log",
            "artifact://files//result.log",
            "artifact://files/",
        ] {
            assert!(
                artifact_locator_relative_path(locator).is_none(),
                "{locator}"
            );
        }
        assert!(artifact_locator_relative_path("artifact://files/result.log").is_some());
    }

    #[test]
    fn rewritten_unavailable_artifact_locator_never_leaks_provider_reference() {
        let mut workflow = extension_test::TestRunWorkflowResult {
            status: "failed".to_string(),
            component: "fixture".to_string(),
            exit_code: 1,
            test_counts: None,
            test_durations: None,
            findings: None,
            failure_analysis_input: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: Some(extension_test::RawTestOutput {
                stdout_tail: "artifact://files/missing.log".to_string(),
                stderr_tail: String::new(),
                truncated: false,
                stdout_truncated: false,
                stderr_truncated: false,
                stdout_seen_bytes: 0,
                stdout_retained_bytes: 0,
                stderr_seen_bytes: 0,
                stderr_retained_bytes: 0,
                stdout_limit_bytes: 0,
                stderr_limit_bytes: 0,
            }),
            extension_phase_timings: vec![homeboy_extension::ExtensionPhaseTiming {
                name: "provider".to_string(),
                duration_ms: 0,
                status: None,
                message: None,
                artifacts: vec![serde_json::json!({
                    "ref": "homeboy://run/run-1/artifact/artifact-1",
                    "available": false,
                    "unavailable_reason": "the declared controller-local artifact file is unavailable",
                })],
                metadata: Default::default(),
            }],
        };

        rewrite_reported_artifact_locators(
            &mut workflow,
            &[ (
                "artifact://files/missing.log".to_string(),
                "homeboy://run/run-1/artifact/artifact-1 (evidence collection unavailable: the declared controller-local artifact file is unavailable)".to_string(),
            )],
        );
        let output = workflow.raw_output.expect("raw output").stdout_tail;
        assert!(!output.contains("artifact://"));
        assert!(output.contains("homeboy://run/run-1/artifact/artifact-1"));
        assert!(output.contains("evidence collection unavailable"));
    }

    #[test]
    fn interrupted_test_observation_persists_parseable_partial_child_evidence() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let run_dir = RunDir::create().expect("run dir");
            std::fs::write(
                run_dir.step_file(homeboy::core::engine::run_dir::files::CHILD_SUPERVISION),
                r#"{
                  "schema":"homeboy.child_supervision.v1",
                  "status":"interrupted",
                  "phase":"child",
                  "command":"silent child",
                  "child_pid":42,
                  "started_at":"2026-01-01T00:00:00Z",
                  "heartbeat_at":"2026-01-01T00:00:01Z",
                  "finished_at":"2026-01-01T00:00:02Z",
                  "timeout_ms":null,
                  "cancellation_reason":"signal:15",
                  "exit_code":143,
                  "stdout_tail":"",
                  "stderr_tail":""
                }"#,
            )
            .expect("write child supervision");
            let observation =
                start_test_observation("homeboy", home.path(), &args, "test", Some(&run_dir))
                    .expect("observation");
            let run_id = observation.active.run_id().to_string();

            finish_test_observation(
                Some(observation),
                &extension_test::TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: "homeboy".to_string(),
                    exit_code: 143,
                    test_counts: None,
                    test_durations: None,
                    findings: None,
                    failure_analysis_input: None,
                    coverage: None,
                    baseline_comparison: None,
                    analysis: None,
                    autofix: None,
                    hints: None,
                    test_scope: None,
                    summary: None,
                    raw_output: None,
                    extension_phase_timings: Vec::new(),
                },
            );

            let store = ObservationStore::open_initialized().expect("store");
            let record = store.get_run(&run_id).expect("read").expect("record");
            assert_eq!(record.status, "error");
            assert_eq!(record.metadata_json["observation_status"], "interrupted");
            assert_eq!(
                record.metadata_json["child_supervision"]["cancellation_reason"],
                "signal:15"
            );
            let artifacts = store.list_artifacts(&run_id).expect("artifacts");
            assert!(artifacts
                .iter()
                .any(|artifact| artifact.kind == "child_supervision"));
            run_dir.cleanup();
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
    fn filter_strips_settings_json_file_aliases() {
        let args = vec![
            "--settings-json-file".to_string(),
            "base.json".to_string(),
            "--settings-profile=profile.json".to_string(),
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
            vec![("runner".to_string(), serde_json::json!("ci"))],
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
#[cfg(test)]
#[path = "../../../../tests/core/extension/component_script_test.rs"]
mod component_script_test;
