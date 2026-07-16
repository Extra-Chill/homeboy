//! Exact run-id fuzz comparison over persisted run artifacts.

use std::fs::File;
use std::path::Path;

use clap::Args;
use serde_json::Value;

use homeboy::core::fuzz::{FuzzResultEnvelope, FUZZ_RESULT_ENVELOPE_SCHEMA};
use homeboy::core::observation::runs_service;
use homeboy::core::observation::{ArtifactRecord, ObservationStore};
use homeboy::core::Error;

use crate::commands::fuzz::{compare_envelopes, FuzzCompareHotspotPolicy};

use super::{CmdResult, RunsOutput};

#[derive(Args, Clone)]
pub struct RunsFuzzCompareArgs {
    /// Baseline fuzz run id.
    #[arg(long = "from-run", value_name = "RUN_ID")]
    pub from_run: String,

    /// Candidate fuzz run id.
    #[arg(long = "to-run", value_name = "RUN_ID")]
    pub to_run: String,

    /// How relative hotspot regressions affect the blocking compare status.
    #[arg(long = "hotspot-policy", value_enum, default_value_t = FuzzCompareHotspotPolicy::Advisory)]
    pub hotspot_policy: FuzzCompareHotspotPolicy,
}

pub fn fuzz_compare_from_args(args: RunsFuzzCompareArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let baseline = resolve_run_fuzz_envelope(&store, &args.from_run)?;
    let candidate = resolve_run_fuzz_envelope(&store, &args.to_run)?;
    let output = compare_envelopes(
        baseline.envelope,
        candidate.envelope,
        args.hotspot_policy,
        baseline.source_ref,
        candidate.source_ref,
        "runs.fuzz-compare",
    )?;
    Ok((RunsOutput::FuzzCompare(output), 0))
}

struct RunFuzzEnvelope {
    envelope: FuzzResultEnvelope,
    source_ref: String,
}

fn resolve_run_fuzz_envelope(
    store: &ObservationStore,
    run_id: &str,
) -> homeboy::core::Result<RunFuzzEnvelope> {
    let _run = runs_service::require_run(store, run_id)?;
    let artifacts = runs_service::list_artifacts_for_run(store, run_id)?;
    let mut candidates = artifacts
        .iter()
        .filter(|artifact| artifact.artifact_type == "file")
        .filter(|artifact| is_fuzz_envelope_candidate(artifact))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|artifact| fuzz_envelope_candidate_rank(artifact));

    for artifact in candidates {
        let Ok(mut envelope) = read_fuzz_envelope_artifact(artifact) else {
            continue;
        };
        attach_related_fuzz_artifacts(&mut envelope, &artifacts);
        return Ok(RunFuzzEnvelope {
            envelope,
            source_ref: format!("homeboy://run/{run_id}/artifact/{}", artifact.id),
        });
    }

    Err(Error::validation_invalid_argument(
        "run_id",
        format!("fuzz result envelope artifact not found for run: {run_id}"),
        Some(run_id.to_string()),
        Some(vec![
            format!("Run `homeboy runs artifacts {run_id}` to inspect recorded artifacts."),
            "Persist a result envelope with `homeboy fuzz report <campaign> --run-id <run-id>`, or ensure the runner writes a homeboy/fuzz-result-envelope/v1 JSON artifact.".to_string(),
        ]),
    ))
}

fn is_fuzz_envelope_candidate(artifact: &ArtifactRecord) -> bool {
    artifact.kind == "fuzz_result_envelope"
        || artifact.metadata_json.get("schema").and_then(Value::as_str)
            == Some(FUZZ_RESULT_ENVELOPE_SCHEMA)
        || artifact.kind == "fuzz_results"
}

fn fuzz_envelope_candidate_rank(artifact: &ArtifactRecord) -> u8 {
    if artifact.kind == "fuzz_result_envelope" {
        0
    } else if artifact.metadata_json.get("schema").and_then(Value::as_str)
        == Some(FUZZ_RESULT_ENVELOPE_SCHEMA)
    {
        1
    } else {
        2
    }
}

fn read_fuzz_envelope_artifact(
    artifact: &ArtifactRecord,
) -> homeboy::core::Result<FuzzResultEnvelope> {
    let path = Path::new(&artifact.path);
    let contents = std::fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(artifact.path.clone())))?;
    let envelope: FuzzResultEnvelope = serde_json::from_str(&contents).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some(format!(
                "parse fuzz result envelope artifact {}",
                artifact.id
            )),
            Some(contents),
        )
    })?;
    if envelope.schema != FUZZ_RESULT_ENVELOPE_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "fuzz result envelope schema must be {FUZZ_RESULT_ENVELOPE_SCHEMA}, got {}",
                envelope.schema
            ),
            Some(envelope.schema),
            None,
        ));
    }
    Ok(envelope)
}

fn attach_related_fuzz_artifacts(envelope: &mut FuzzResultEnvelope, artifacts: &[ArtifactRecord]) {
    let values = artifacts
        .iter()
        .filter(|artifact| artifact.artifact_type == "file")
        .filter(|artifact| artifact.kind != "fuzz_result_envelope")
        .filter_map(read_related_fuzz_artifact_json)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return;
    }
    let mut metadata = match std::mem::take(&mut envelope.metadata) {
        Value::Object(object) => object,
        value if value.is_null() => serde_json::Map::new(),
        value => {
            let mut object = serde_json::Map::new();
            object.insert("original".to_string(), value);
            object
        }
    };
    metadata.insert("resolved_run_artifacts".to_string(), Value::Array(values));
    envelope.metadata = Value::Object(metadata);
}

fn read_related_fuzz_artifact_json(artifact: &ArtifactRecord) -> Option<Value> {
    let path = Path::new(&artifact.path);
    let file = File::open(path).ok()?;
    let json = serde_json::from_reader::<_, Value>(file).ok()?;
    is_related_fuzz_json(&json).then(|| json)
}

fn is_related_fuzz_json(value: &Value) -> bool {
    value
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|schema| {
            schema.contains("fuzz-hotspot") || schema.contains("fuzz-observation")
        })
        || value.get("hotspots").is_some()
        || value.get("observation_set").is_some()
        || value.get("observations").is_some()
}

#[cfg(test)]
mod tests {
    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
    use homeboy::test_support::with_isolated_home;

    use super::*;

    fn sample_run() -> NewRunRecord {
        NewRunRecord::builder("fuzz")
            .component_id("homeboy")
            .command("homeboy fuzz run homeboy")
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .metadata(serde_json::json!({}))
            .build()
    }

    fn envelope(id: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": "homeboy/fuzz-result-envelope/v1",
            "version": 1,
            "id": id,
            "status": "passed",
            "request": {
                "schema": "homeboy/fuzz-execution-request/v1",
                "version": 1,
                "id": format!("{id}-request"),
                "component": "homeboy"
            }
        })
    }

    #[test]
    fn fuzz_compare_resolves_envelopes_from_run_artifacts() {
        with_isolated_home(|home| {
            homeboy::core::set_artifact_root_override(Some(home.path().join("artifacts")));
            let store = ObservationStore::open_initialized().expect("store");
            let baseline = store.start_run(sample_run()).expect("baseline run");
            store
                .finish_run(&baseline.id, RunStatus::Pass, None)
                .expect("finish baseline");
            let candidate = store.start_run(sample_run()).expect("candidate run");
            store
                .finish_run(&candidate.id, RunStatus::Pass, None)
                .expect("finish candidate");

            let baseline_path = home.path().join("baseline-envelope.json");
            let candidate_path = home.path().join("candidate-envelope.json");
            std::fs::write(
                &baseline_path,
                serde_json::to_string(&envelope("baseline")).unwrap(),
            )
            .expect("baseline envelope");
            std::fs::write(
                &candidate_path,
                serde_json::to_string(&envelope("candidate")).unwrap(),
            )
            .expect("candidate envelope");
            store
                .record_artifact(&baseline.id, "fuzz_result_envelope", &baseline_path)
                .expect("baseline artifact");
            store
                .record_artifact(&candidate.id, "fuzz_result_envelope", &candidate_path)
                .expect("candidate artifact");

            let (_, exit_code) = fuzz_compare_from_args(RunsFuzzCompareArgs {
                from_run: baseline.id.clone(),
                to_run: candidate.id.clone(),
                hotspot_policy: FuzzCompareHotspotPolicy::Advisory,
            })
            .expect("fuzz compare");

            assert_eq!(exit_code, 0);
            homeboy::core::set_artifact_root_override(None);
        });
    }
}
