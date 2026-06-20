//! Trace observation lifecycle: dispatch observation start/finish and run
//! workflow result/error persistence.
//!
//! Owns the `ObservationStore` interactions that bracket a trace run — opening
//! a run record for Lab dispatch, finishing it, and persisting workflow
//! results, spans, and artifact validation. Split out of the trace command
//! root to keep it under its structural line threshold. The parent re-exports
//! the externally consumed `start_lab_dispatch_observation` /
//! `finish_lab_dispatch_observation` entry points.

use std::path::Path;

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::observation::{
    NewRunRecord, NewTraceRunRecord, NewTraceSpanRecord, ObservationStore, RunStatus,
};
use homeboy::core::rig;

use super::observations::{self, record_trace_artifacts};
use super::{
    load_rig_context, resolve_component_id, resolve_trace_profile_args, rig_component_path,
    trace_scenario, PersistedRunRetrieval, TraceArgs,
};

pub(super) struct ActiveTraceObservation {
    pub(super) store: ObservationStore,
    pub(super) run_id: String,
    pub(super) component_id: String,
    pub(super) rig_id: Option<String>,
    pub(super) scenario_id: String,
}

pub(crate) struct LabTraceDispatchObservation {
    pub(super) store: ObservationStore,
    pub(super) run_id: String,
    pub(super) component_id: String,
    pub(super) rig_id: Option<String>,
    pub(super) scenario_id: String,
}

impl homeboy::core::lab_routing::LabDispatchObserver for LabTraceDispatchObservation {
    fn run_id(&self) -> Option<&str> {
        Some(self.run_id.as_str())
    }

    fn finish(
        self: Box<Self>,
        status: RunStatus,
        metadata: serde_json::Value,
    ) -> Option<PersistedRunRetrieval> {
        finish_lab_dispatch_observation(Some(*self), status, metadata)
    }
}

pub(crate) fn start_lab_dispatch_observation(
    args: &TraceArgs,
    normalized_args: &[String],
    runner_id: Option<&str>,
) -> Option<LabTraceDispatchObservation> {
    let mut resolved = args.clone();
    resolve_trace_profile_args(&mut resolved).ok()?;
    let scenario_id = trace_scenario(&resolved).ok()?;
    if scenario_id == "list" {
        return None;
    }
    let scenario_id = scenario_id.to_string();
    let rig_context = load_rig_context(resolved.rig.as_deref()).ok()?;
    let component_id = resolve_component_id(
        &resolved.comp,
        rig_context.as_ref().map(|context| &context.rig_spec),
    )
    .ok()?;
    let component_path = resolved.comp.path.clone().or_else(|| {
        rig_context
            .as_ref()
            .and_then(|context| rig_component_path(&context.rig_spec, &component_id))
    });
    let store = ObservationStore::open_initialized().ok()?;
    let cwd = std::env::current_dir().ok();
    let lab_dispatch_metadata = || {
        serde_json::json!({
            "phase": "route_before_lab_dispatch",
            "runner_id": runner_id,
            "status": "running"
        })
    };
    let run =
        store
            .start_run(
                NewRunRecord::builder("trace")
                    .component_id(component_id.clone())
                    .command(normalized_args.join(" "))
                    .optional_cwd_path(cwd.as_deref())
                    .current_homeboy_version()
                    .git_sha(component_path.as_deref().and_then(|path| {
                        homeboy::core::git::short_head_revision_at(Path::new(path))
                    }))
                    .optional_rig_id(resolved.rig.clone())
                    .metadata(serde_json::json!({
                        "scenario_id": scenario_id,
                        "component_path": component_path,
                        "lab_dispatch": lab_dispatch_metadata()
                    }))
                    .build(),
            )
            .ok()?;
    let observation = LabTraceDispatchObservation {
        store,
        run_id: run.id,
        component_id,
        rig_id: resolved.rig.clone(),
        scenario_id,
    };
    let _ = observation.store.record_trace_run(
        NewTraceRunRecord::builder(
            &observation.run_id,
            &observation.component_id,
            &observation.scenario_id,
            RunStatus::Running.as_str(),
        )
        .trace_rig_id(observation.rig_id.as_deref())
        .metadata(serde_json::json!({
            "lab_dispatch": lab_dispatch_metadata()
        }))
        .build(),
    );
    Some(observation)
}

pub(crate) fn finish_lab_dispatch_observation(
    observation: Option<LabTraceDispatchObservation>,
    status: RunStatus,
    metadata: serde_json::Value,
) -> Option<PersistedRunRetrieval> {
    let observation = observation?;
    let retrieval = PersistedRunRetrieval::for_run(&observation.run_id);
    let _ = observation.store.record_trace_run(
        NewTraceRunRecord::builder(
            &observation.run_id,
            &observation.component_id,
            &observation.scenario_id,
            status.as_str(),
        )
        .trace_rig_id(observation.rig_id.as_deref())
        .metadata(metadata.clone())
        .build(),
    );
    let _ = observation
        .store
        .finish_run(&observation.run_id, status, Some(metadata));
    Some(retrieval)
}

pub(super) fn persist_trace_workflow_result(
    observation: &ActiveTraceObservation,
    run_dir: &RunDir,
    workflow: &extension_trace::TraceRunWorkflowResult,
    rig_state: Option<&rig::RigStateSnapshot>,
) {
    let run_status = trace_run_status(workflow);
    let baseline_status = baseline_status(workflow);
    let results = workflow.results.as_ref();
    let trace_scenario_id = results
        .map(|results| results.scenario_id.clone())
        .unwrap_or_else(|| observation.scenario_id.clone());
    let _ = observation.store.record_trace_run(
        NewTraceRunRecord::builder(
            &observation.run_id,
            &observation.component_id,
            trace_scenario_id,
            run_status.as_str(),
        )
        .trace_rig_id(observation.rig_id.as_deref())
        .baseline_status(baseline_status.as_deref())
        .metadata(serde_json::json!({
            "status": &workflow.status,
            "exit_code": workflow.exit_code,
            "summary": results.and_then(|results| results.summary.clone()),
            "failure": &workflow.failure,
            "overlays": &workflow.overlays,
            "baseline_comparison": &workflow.baseline_comparison,
            "baseline_status": baseline_status,
            "hints": &workflow.hints,
            "rig_state": rig_state,
            "assertion_count": results.map(|results| results.assertions.len()).unwrap_or(0),
            "artifact_count": results.map(|results| results.artifacts.len()).unwrap_or(0),
            "span_count": results.map(|results| results.span_results.len()).unwrap_or(0),
        }))
        .build(),
    );

    if let Some(results) = results {
        for span in &results.span_results {
            let _ = observation.store.record_trace_span(
                NewTraceSpanRecord::builder(
                    &observation.run_id,
                    &span.id,
                    format!("{:?}", span.status).to_ascii_lowercase(),
                )
                .duration_ms(span.duration_ms.map(|value| value as f64))
                .from_event(Some(&span.from))
                .to_event(Some(&span.to))
                .metadata(serde_json::json!({
                    "from_t_ms": span.from_t_ms,
                    "to_t_ms": span.to_t_ms,
                    "missing": span.missing,
                    "message": &span.message,
                }))
                .build(),
            );
        }
    }

    let artifact_observation =
        record_trace_artifacts(&observation.store, &observation.run_id, run_dir, results);
    let finish_status =
        if run_status == RunStatus::Pass && artifact_observation.has_declared_artifact_failures() {
            RunStatus::Fail
        } else {
            run_status
        };
    let _ = observation.store.finish_run(
        &observation.run_id,
        finish_status,
        Some(trace_run_finish_metadata(
            workflow,
            Some(&artifact_observation),
        )),
    );
}

pub(super) fn persist_trace_workflow_error(
    observation: &ActiveTraceObservation,
    run_dir: &RunDir,
    error: &homeboy::core::Error,
) {
    let error_metadata = serde_json::json!({
        "error": {
            "code": error.code.as_str(),
            "message": &error.message,
            "details": &error.details,
        }
    });
    let _ = observation.store.record_trace_run(
        NewTraceRunRecord::builder(
            &observation.run_id,
            &observation.component_id,
            &observation.scenario_id,
            RunStatus::Error.as_str(),
        )
        .trace_rig_id(observation.rig_id.as_deref())
        .metadata(error_metadata.clone())
        .build(),
    );
    let artifact_observation =
        record_trace_artifacts(&observation.store, &observation.run_id, run_dir, None);
    let _ = observation.store.finish_run(
        &observation.run_id,
        RunStatus::Error,
        Some(merge_trace_artifact_metadata(
            error_metadata,
            &artifact_observation,
        )),
    );
}

fn trace_run_status(workflow: &extension_trace::TraceRunWorkflowResult) -> RunStatus {
    if workflow.failure.is_some() || workflow.status == "error" {
        RunStatus::Error
    } else if workflow.exit_code == 0 && workflow.status == "pass" {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    }
}

fn baseline_status(workflow: &extension_trace::TraceRunWorkflowResult) -> Option<String> {
    workflow.baseline_comparison.as_ref().map(|comparison| {
        if comparison.regression {
            "regression"
        } else if comparison.has_improvements {
            "improvement"
        } else {
            "pass"
        }
        .to_string()
    })
}

fn trace_run_finish_metadata(
    workflow: &extension_trace::TraceRunWorkflowResult,
    artifact_observation: Option<&observations::TraceArtifactObservationResult>,
) -> serde_json::Value {
    let metadata = serde_json::json!({
        "status": &workflow.status,
        "exit_code": workflow.exit_code,
        "failure": &workflow.failure,
        "overlays": &workflow.overlays,
        "baseline_comparison": &workflow.baseline_comparison,
        "hints": &workflow.hints,
        "results": &workflow.results,
    });
    if let Some(artifact_observation) = artifact_observation {
        merge_trace_artifact_metadata(metadata, artifact_observation)
    } else {
        metadata
    }
}

fn merge_trace_artifact_metadata(
    mut metadata: serde_json::Value,
    artifact_observation: &observations::TraceArtifactObservationResult,
) -> serde_json::Value {
    if !artifact_observation.has_declared_artifact_failures() {
        return metadata;
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "artifact_validation".to_string(),
            serde_json::json!({
                "status": "fail",
                "missing_declared_artifacts": artifact_observation.missing_declared_artifacts,
                "invalid_declared_artifacts": artifact_observation.invalid_declared_artifacts,
            }),
        );
    }
    metadata
}
