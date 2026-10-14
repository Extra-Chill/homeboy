//! Execute one workload of a declared kind and persist it.
//!
//! The run is an `ActiveObservation` of kind `<kind>`, started before the
//! runner launches and always terminalized. The runner's envelope is the
//! run's `workload_result` artifact, every file the envelope lists is a
//! `workload_artifact`, and its findings go to the shared findings store.

use std::path::Path;
use std::time::Duration;

use homeboy_engine_primitives::baseline::BaselineFlags;
use homeboy_engine_primitives::workload_compare::{apply_baseline_flags, BaselineSlot};
use homeboy_extension_contract::{WorkloadResult, WorkloadStatus, WORKLOAD_RESULT_SCHEMA};
use serde::Serialize;
use uuid::Uuid;

use crate::engine::run_dir::RunDir;
use crate::error::{Error, Result};
use crate::extension::invoke::{
    build_scenario_runner, provision_declared_helpers, ScenarioRunnerOptions,
};
use crate::observation::{
    finding_records_from_homeboy_findings, ActiveObservation, NewRunRecord, RunRecord, RunStatus,
};

use super::compare::{WorkloadAdapter, WorkloadComparison};
use super::resolve::ResolvedWorkloadKind;
use super::safety::DestructiveAuthorization;
use super::{
    absolute_workload_path, baseline_key, env, safety, RigWorkloadFile,
    SAFETY_CLASS_SOURCE_METADATA_KEY, WORKLOAD_ARTIFACT_KIND, WORKLOAD_COMPARE_METADATA_KEY,
    WORKLOAD_ISOLATION_PROOF_ARTIFACT_KIND, WORKLOAD_PATH_METADATA_KEY,
    WORKLOAD_RESULT_ARTIFACT_KIND,
};

pub struct WorkloadRunRequest<'a> {
    pub resolved: &'a ResolvedWorkloadKind,
    pub workload_id: Option<&'a str>,
    /// Caller-owned stable run id; a `<kind>-<uuid>` id is minted when absent.
    pub run_id: Option<&'a str>,
    pub seed: Option<&'a str>,
    pub rig_id: Option<&'a str>,
    /// Rig-owned workload files for the kind's owning extension, resolved by
    /// the caller. The selected one is exported as `HOMEBOY_WORKLOAD_PATH`
    /// and all of them as `HOMEBOY_WORKLOAD_EXTRA_PATHS`.
    pub rig_workloads: &'a [RigWorkloadFile],
    pub settings: &'a [(String, String)],
    pub settings_json: &'a [(String, serde_json::Value)],
    pub script_args: &'a [String],
    pub timeout: Option<Duration>,
    pub authorization: Option<&'a DestructiveAuthorization>,
    pub baseline: BaselineFlags,
    /// The invocation recorded on the run row.
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkloadRunOutcome {
    pub run_id: String,
    pub status: String,
    pub exit_code: i32,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<WorkloadResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<WorkloadComparison>,
    pub runner_exit_code: i32,
    pub runner_timed_out: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    /// Retained only when the run failed.
    pub run_dir: String,
    #[serde(skip)]
    pub run: RunRecord,
}

pub fn effective_run_id(kind: &str, requested: Option<&str>) -> String {
    requested
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{kind}-{}", Uuid::new_v4()))
}

pub(super) fn tail(text: &str) -> String {
    const MAX: usize = 4000;
    if text.len() <= MAX {
        return text.to_string();
    }
    let mut start = text.len() - MAX;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// Parse and check the runner's envelope against the request.
pub fn read_envelope(
    results_file: &Path,
    artifact_dir: &Path,
    kind: &str,
    result_schema: Option<&str>,
    requested_workload: Option<&str>,
) -> std::result::Result<WorkloadResult, String> {
    let bytes = std::fs::read(results_file).map_err(|_| {
        format!(
            "runner wrote no {WORKLOAD_RESULT_SCHEMA} envelope to {}",
            env::RESULTS_FILE
        )
    })?;
    let mut result = WorkloadResult::parse(&bytes, kind, result_schema)?;
    match (requested_workload, result.workload_id.as_deref()) {
        (Some(requested), Some(reported)) if requested != reported => {
            return Err(format!(
                "envelope reports workload `{reported}` but `{requested}` was requested"
            ))
        }
        (Some(requested), None) => result.workload_id = Some(requested.to_string()),
        _ => {}
    }
    for artifact in &result.artifacts {
        if !artifact_dir.join(&artifact.path).exists() {
            return Err(format!(
                "envelope lists artifact `{}` at {} but the runner did not write it",
                artifact.name, artifact.path
            ));
        }
    }
    for case in &result.cases {
        if let Some(reference) = case.artifact_ref.as_deref() {
            if result.artifact(reference).is_none()
                && !is_relative_artifact(artifact_dir, reference)
            {
                return Err(format!(
                    "case `{}` artifact_ref `{reference}` names no listed artifact or file",
                    case.case_id
                ));
            }
        }
    }
    Ok(result)
}

fn is_relative_artifact(artifact_dir: &Path, reference: &str) -> bool {
    homeboy_extension_contract::workload_result::is_contained_relative_path(reference)
        && artifact_dir.join(reference).exists()
}

/// The terminal status of a run from its runner outcome and envelope.
pub fn outcome_status(
    runner_success: bool,
    runner_exit_code: i32,
    timed_out: bool,
    result: Option<&WorkloadResult>,
    result_error: &mut Option<String>,
) -> (RunStatus, i32) {
    let failed_exit = if runner_exit_code != 0 {
        runner_exit_code
    } else {
        1
    };
    if timed_out {
        result_error.get_or_insert_with(|| "runner timed out".to_string());
        return (RunStatus::Error, failed_exit);
    }
    let Some(result) = result else {
        return (RunStatus::Error, failed_exit);
    };
    match result.status {
        WorkloadStatus::Pass | WorkloadStatus::Skipped if runner_success => (
            if result.status == WorkloadStatus::Pass {
                RunStatus::Pass
            } else {
                RunStatus::Skipped
            },
            0,
        ),
        WorkloadStatus::Pass | WorkloadStatus::Skipped => {
            *result_error = Some(format!(
                "runner exited {runner_exit_code} but its envelope reports {}",
                result.status.as_str()
            ));
            (RunStatus::Error, failed_exit)
        }
        WorkloadStatus::Fail => (RunStatus::Fail, failed_exit),
        WorkloadStatus::Error => (RunStatus::Error, failed_exit),
    }
}

pub fn run(request: WorkloadRunRequest<'_>) -> Result<WorkloadRunOutcome> {
    let resolved = request.resolved;
    let kind = resolved.kind.as_str();
    if resolved.extension.script_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            format!("workloads.{kind}.script"),
            format!(
                "extension `{}` declares workload kind `{kind}` without a runner script",
                resolved.extension_id()
            ),
            None,
            None,
        ));
    }
    let run_id = effective_run_id(kind, request.run_id);
    let run_dir = RunDir::create()?;
    let results_file = run_dir.step_file("workload-result.json");
    let artifact_dir = run_dir.step_file("workload-artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|error| {
        Error::internal_io(error.to_string(), Some(artifact_dir.display().to_string()))
    })?;
    let source_path = resolved.context.source_path.to_string_lossy().to_string();
    let component_revision = crate::git::short_head_revision_at(&resolved.context.source_path);
    let cwd = std::env::current_dir().ok();
    // The selected rig workload raises the kind's safety class when its file
    // declares destructive; the caller authorized the effective class.
    let selected_rig_workload =
        super::selected_rig_workload(request.rig_workloads, request.workload_id);
    let effective_safety = safety::effective_safety_class(
        resolved.config.safety_class,
        selected_rig_workload.map(|file| file.path.as_path()),
    );
    let mut metadata = serde_json::json!({
        "source": "homeboy workload run",
        "status": "running",
        "workload_kind": kind,
        "workload_id": request.workload_id,
        "extension_id": resolved.extension_id(),
        "source_path": source_path,
        "seed": request.seed,
        "safety_class": effective_safety.class.as_str(),
        "destructive_authorized": request.authorization.is_some(),
        "component_revision": component_revision,
    });
    metadata[SAFETY_CLASS_SOURCE_METADATA_KEY] = serde_json::json!(effective_safety.source);
    if let Some(file) = selected_rig_workload {
        let workload_path = absolute_workload_path(&file.path);
        metadata[WORKLOAD_PATH_METADATA_KEY] =
            serde_json::json!(workload_path.to_string_lossy().to_string());
    }
    metadata[WORKLOAD_COMPARE_METADATA_KEY] =
        serde_json::to_value(&resolved.config.compare).unwrap_or_default();
    let observation = ActiveObservation::start_with_stable_id(
        NewRunRecord::builder(kind)
            .component_id(&resolved.context.component_id)
            .command(request.command.clone())
            .optional_cwd_path(cwd.as_deref())
            .homeboy_version(homeboy_product_identity::product_version())
            .git_sha(component_revision.clone())
            .optional_rig_id(request.rig_id)
            .metadata(metadata)
            .build(),
        &run_id,
    )?;
    let _ = run_dir.bind_run_id(&run_id);
    let interrupted = |error: Error| {
        observation.finish_error_with_merged_metadata(serde_json::json!({
            "status": "error",
            "error": error.message,
        }));
        run_dir.finish(false);
        error
    };

    let helpers =
        provision_declared_helpers(&resolved.config.runtime_helpers).map_err(&interrupted)?;
    let rig_workload_paths = request
        .rig_workloads
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let mut runner = build_scenario_runner(ScenarioRunnerOptions {
        execution_context: &resolved.extension,
        component: &resolved.context.component,
        path_override: Some(source_path.clone()),
        settings: request.settings,
        settings_json: request.settings_json,
        run_dir: &run_dir,
        results_env: Some((env::RESULTS_FILE, results_file.clone())),
        scenario_env: request.workload_id.map(|id| (env::ID, id)),
        artifact_env: Some((env::ARTIFACT_DIR, &artifact_dir)),
        list_only_env: Some((env::LIST_ONLY, false)),
        extra_workloads_env: Some((env::EXTRA_PATHS, &rig_workload_paths, "workloads")),
        env_provider_extensions: &[],
        invocation_requirements: Default::default(),
    })
    .map_err(&interrupted)?
    .env(env::KIND, kind)
    .env(env::RUN_ID, &run_id)
    .env_opt(env::SEED, &request.seed.map(str::to_string))
    .env_opt(
        env::PATH,
        &selected_rig_workload.map(|file| {
            absolute_workload_path(&file.path)
                .to_string_lossy()
                .to_string()
        }),
    )
    .timeout(request.timeout)
    .script_args(request.script_args)
    .passthrough(false);
    for (key, value) in safety::env(request.authorization) {
        runner = runner.env(&key, &value);
    }
    for helper in &helpers {
        runner = runner.env(&helper.env_var, &helper.path);
    }
    if !helpers.is_empty() {
        let provenance = serde_json::to_string(&helpers).map_err(|error| {
            interrupted(Error::internal_json(
                error.to_string(),
                Some("serialize runtime helper provenance".to_string()),
            ))
        })?;
        runner = runner.env("HOMEBOY_RUNTIME_HELPERS_PROVENANCE", &provenance);
    }
    let output = runner.run().map_err(&interrupted)?;

    let mut result_error = None;
    let result = match read_envelope(
        &results_file,
        &artifact_dir,
        kind,
        resolved.config.result_schema.as_deref(),
        request.workload_id,
    ) {
        Ok(result) => Some(result),
        Err(problem) => {
            result_error = Some(problem);
            None
        }
    };
    let (status, mut exit_code) = outcome_status(
        output.success,
        output.exit_code,
        output.timed_out,
        result.as_ref(),
        &mut result_error,
    );

    let store = observation.store();
    if results_file.is_file() {
        store
            .record_artifact_with_metadata(
                &run_id,
                WORKLOAD_RESULT_ARTIFACT_KIND,
                &results_file,
                serde_json::json!({
                    "schema": WORKLOAD_RESULT_SCHEMA,
                    "valid": result.is_some(),
                    "error": result_error,
                }),
            )
            .map_err(&interrupted)?;
    }
    if let Some(result) = result.as_ref() {
        for artifact in &result.artifacts {
            let path = artifact_dir.join(&artifact.path);
            let metadata = serde_json::json!({
                "name": artifact.name,
                "relative_path": artifact.path,
                "mime": artifact.mime,
            });
            let recorded = if path.is_dir() {
                store.record_directory_artifact_with_metadata(
                    &run_id,
                    WORKLOAD_ARTIFACT_KIND,
                    &path,
                    metadata,
                )
            } else {
                store.record_artifact_with_metadata(
                    &run_id,
                    WORKLOAD_ARTIFACT_KIND,
                    &path,
                    metadata,
                )
            };
            recorded.map_err(&interrupted)?;
        }
        let findings = result.findings.iter().cloned().map(|mut finding| {
            finding
                .metadata
                .insert("workload_kind".to_string(), serde_json::json!(kind));
            if let Some(workload_id) = &result.workload_id {
                finding
                    .metadata
                    .insert("workload_id".to_string(), serde_json::json!(workload_id));
            }
            finding
        });
        store
            .record_findings(&finding_records_from_homeboy_findings(&run_id, findings))
            .map_err(&interrupted)?;
    }
    if let Some(authorization) = request.authorization {
        observation.record_artifact_if_file(
            WORKLOAD_ISOLATION_PROOF_ARTIFACT_KIND,
            &authorization.isolation_proof_file,
        );
    }

    let baseline_comparison = match result.as_ref() {
        Some(result) => {
            let slot_key = baseline_key(kind);
            let slot = BaselineSlot::new(&resolved.context.source_path, request.rig_id)
                .with_kind(&slot_key);
            let gate = apply_baseline_flags::<WorkloadAdapter>(
                &slot,
                &resolved.context.component_id,
                &request.baseline,
                Some(result),
                status == RunStatus::Pass,
                &resolved.config.compare,
            )
            .map_err(&interrupted)?;
            if let Some(exit) = gate.exit_override {
                exit_code = exit;
            }
            gate.comparison.map(WorkloadComparison::new)
        }
        None => None,
    };
    let success = exit_code == 0;

    let finished = observation
        .try_finish_with_merged_metadata(
            status,
            serde_json::json!({
                "status": status.as_str(),
                "exit_code": exit_code,
                "success": success,
                "workload_id": result.as_ref().and_then(|result| result.workload_id.clone()).or(request.workload_id.map(str::to_string)),
                "result_status": result.as_ref().map(|result| result.status.as_str()),
                "result_error": result_error,
                "case_totals": result.as_ref().map(WorkloadResult::case_totals),
                "finding_count": result.as_ref().map(|result| result.findings.len()),
                "metrics": result.as_ref().map(|result| result.metrics.clone()),
                "runner_exit_code": output.exit_code,
                "runner_timed_out": output.timed_out,
                "baseline": baseline_comparison.as_ref().map(|comparison| serde_json::json!({
                    "status": comparison.status,
                    "regressions": comparison.regressions,
                })),
            }),
        )
        .map_err(&interrupted)?;
    run_dir.finish(success);

    Ok(WorkloadRunOutcome {
        run_id,
        status: status.as_str().to_string(),
        exit_code,
        success,
        result,
        result_error,
        baseline_comparison,
        runner_exit_code: output.exit_code,
        runner_timed_out: output.timed_out,
        stdout_tail: tail(&output.stdout),
        stderr_tail: tail(&output.stderr),
        run_dir: if success {
            String::new()
        } else {
            run_dir.path().to_string_lossy().to_string()
        },
        run: finished,
    })
}
