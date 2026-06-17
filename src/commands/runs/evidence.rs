use std::fs;
use std::path::Path;

use homeboy::core::artifact_ref::{ArtifactRef, EvidenceRef};
use homeboy::core::observation::{runs_service, ArtifactRecord, ObservationStore, RunRecord};
use serde::Serialize;
use serde_json::Value;

use super::{disk, reconcile, require_run, run_summary, CmdResult, RunSummary, RunsOutput};

#[derive(Serialize)]
pub struct RunsEvidenceOutput {
    pub command: &'static str,
    pub run_id: String,
    pub run: RunSummary,
    pub homeboy_version: Option<String>,
    pub metadata: RunsEvidenceMetadata,
    pub heartbeat: RunsEvidenceHeartbeat,
    pub artifact_index: RunsEvidenceArtifactIndex,
    pub retention: RunsEvidenceRetention,
    pub failure: RunsEvidenceFailureSummary,
    pub disk_budget: RunsEvidenceDiskBudget,
    pub evidence_links: Vec<RunsEvidenceLink>,
}

#[derive(Serialize)]
pub struct RunsEvidenceMetadata {
    pub cost: Value,
    pub timing: Value,
    pub version: Value,
    pub host: Value,
    pub runtime: Value,
}

#[derive(Serialize)]
pub struct RunsEvidenceHeartbeat {
    pub status: String,
    pub stale: bool,
    pub stale_reason: Option<String>,
    pub owner_pid: Option<u32>,
    pub updated_at: String,
}

#[derive(Serialize)]
pub struct RunsEvidenceArtifactIndex {
    pub count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub url_count: usize,
    pub missing_count: usize,
    pub total_size_bytes: u64,
    pub artifacts: Vec<RunsEvidenceArtifact>,
}

#[derive(Serialize)]
pub struct RunsEvidenceArtifact {
    #[serde(rename = "ref")]
    pub reference: ArtifactRef,
    pub id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    pub url: Option<String>,
    pub public: bool,
    pub public_url: Option<String>,
    pub relative_to: Option<String>,
    pub fetch_command: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub created_at: String,
    pub exists: bool,
    pub retention_candidate: bool,
}

#[derive(Serialize)]
pub struct RunsEvidenceRetention {
    pub artifact_root: String,
    pub default_retention_days: i64,
    pub cleanup_command: String,
}

#[derive(Serialize)]
pub struct RunsEvidenceFailureSummary {
    pub failed: bool,
    pub status: String,
    pub exit_code: Option<i64>,
    pub error: Option<String>,
    pub failure: Value,
    pub gate_failures: Vec<String>,
    pub hints: Vec<String>,
}

pub type RunsEvidenceDiskBudget = disk::DiskBudget;

#[derive(Serialize)]
pub struct RunsEvidenceLink {
    #[serde(rename = "ref")]
    pub reference: EvidenceRef,
    pub kind: String,
    pub target: String,
    pub label: String,
}

pub fn evidence(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let run = require_run(&store, run_id)?;
    let artifacts = runs_service::enrich_artifact_links(store.list_artifacts(run_id)?);
    let artifact_root = homeboy::core::artifacts::root()?;
    let artifact_index = evidence_artifact_index(&artifacts);
    let disk_budget = disk::disk_budget(
        &artifact_root,
        "artifact",
        "disk budget probing is not implemented for this platform",
    );
    let stale_reason = reconcile::running_status_note(&run);
    let metadata = evidence_metadata(&run.metadata_json);
    let failure = evidence_failure_summary(&run);
    let links = evidence_links(&artifacts);

    Ok((
        RunsOutput::Evidence(RunsEvidenceOutput {
            command: "runs.evidence",
            run_id: run.id.clone(),
            run: run_summary(run.clone()),
            homeboy_version: run.homeboy_version.clone(),
            metadata,
            heartbeat: RunsEvidenceHeartbeat {
                status: run.status.clone(),
                stale: stale_reason.is_some(),
                stale_reason,
                owner_pid: homeboy::core::observation::run_owner_pid(&run),
                updated_at: run
                    .finished_at
                    .clone()
                    .unwrap_or_else(|| run.started_at.clone()),
            },
            artifact_index,
            retention: RunsEvidenceRetention {
                artifact_root: artifact_root.display().to_string(),
                default_retention_days: 30,
                cleanup_command: format!(
                    "homeboy runs artifact cleanup-persisted --run-id {} --older-than-days 30",
                    run.id
                ),
            },
            failure,
            disk_budget,
            evidence_links: links,
        }),
        0,
    ))
}

fn evidence_metadata(metadata: &Value) -> RunsEvidenceMetadata {
    RunsEvidenceMetadata {
        cost: pick_metadata(metadata, &["cost", "costs", "usage", "token_usage"]),
        timing: pick_metadata(
            metadata,
            &[
                "timing",
                "timings",
                "duration",
                "scenario_metrics",
                "phase_events",
                "phase_summaries",
                "failure_classification",
            ],
        ),
        version: pick_metadata(metadata, &["version", "versions", "homeboy_version"]),
        host: pick_metadata(
            metadata,
            &["host", "hostname", "machine", "resource_policy"],
        ),
        runtime: pick_metadata(metadata, &["runtime", "runner", "ci_context", "rig_state"]),
    }
}

fn pick_metadata(metadata: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(value) = metadata.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

fn evidence_artifact_index(artifacts: &[ArtifactRecord]) -> RunsEvidenceArtifactIndex {
    let mut file_count = 0;
    let mut directory_count = 0;
    let mut url_count = 0;
    let mut missing_count = 0;
    let mut total_size_bytes = 0u64;
    let artifacts = artifacts
        .iter()
        .map(|artifact| {
            let reference = artifact_ref(artifact);
            let public_url = artifact_public_url(artifact);
            let exists = artifact_exists(artifact);
            if !exists {
                missing_count += 1;
            }
            match artifact.artifact_type.as_str() {
                "file" => file_count += 1,
                "directory" => directory_count += 1,
                "url" => url_count += 1,
                _ => {}
            }
            let size = artifact_size_bytes(artifact);
            total_size_bytes = total_size_bytes.saturating_add(size);
            RunsEvidenceArtifact {
                id: reference.id.clone(),
                kind: reference.kind.clone(),
                artifact_type: reference.artifact_type.clone(),
                path: reference.path.clone(),
                url: artifact
                    .url
                    .clone()
                    .or_else(|| (artifact.artifact_type == "url").then(|| artifact.path.clone())),
                public: public_url.is_some() || artifact.artifact_type == "url",
                public_url,
                relative_to: artifact_relative_to(artifact),
                fetch_command: artifact_fetch_command(artifact),
                size_bytes: artifact.size_bytes,
                sha256: artifact.sha256.clone(),
                created_at: artifact.created_at.clone(),
                exists,
                retention_candidate: artifact.artifact_type != "url",
                reference,
            }
        })
        .collect::<Vec<_>>();

    RunsEvidenceArtifactIndex {
        count: artifacts.len(),
        file_count,
        directory_count,
        url_count,
        missing_count,
        total_size_bytes,
        artifacts,
    }
}

fn artifact_public_url(artifact: &ArtifactRecord) -> Option<String> {
    if artifact.artifact_type == "url" {
        return artifact_ref(artifact).public_target();
    }
    artifact.public_url.clone().or_else(|| {
        artifact
            .metadata_json
            .get("public_url")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn artifact_ref(artifact: &ArtifactRecord) -> ArtifactRef {
    let mut reference = ArtifactRef::from_record(artifact);
    if reference.public_url.is_none() {
        reference.public_url = artifact
            .metadata_json
            .get("public_url")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    reference
}

fn artifact_relative_to(artifact: &ArtifactRecord) -> Option<String> {
    if artifact.artifact_type == "url" || artifact_public_url(artifact).is_some() {
        return None;
    }
    if artifact.artifact_type == "file" || artifact.artifact_type == "remote_file" {
        return Some("homeboy observation artifact store".to_string());
    }
    artifact
        .metadata_json
        .get("source")
        .and_then(Value::as_str)
        .map(|source| format!("{source} metadata"))
}

fn artifact_fetch_command(artifact: &ArtifactRecord) -> Option<String> {
    if artifact.artifact_type == "file" || artifact.artifact_type == "remote_file" {
        return Some(format!(
            "homeboy runs artifact get {} {} -o <path>",
            artifact.run_id, artifact.id
        ));
    }
    None
}

fn artifact_exists(artifact: &ArtifactRecord) -> bool {
    if artifact.artifact_type == "url" {
        return true;
    }
    Path::new(&artifact.path).exists()
}

fn artifact_size_bytes(artifact: &ArtifactRecord) -> u64 {
    if let Some(size) = artifact
        .size_bytes
        .and_then(|size| u64::try_from(size).ok())
    {
        return size;
    }
    let path = Path::new(&artifact.path);
    if path.is_file() {
        return fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    }
    if path.is_dir() {
        return directory_size_bytes(path);
    }
    0
}

fn directory_size_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                directory_size_bytes(&path)
            } else {
                fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn evidence_failure_summary(run: &RunRecord) -> RunsEvidenceFailureSummary {
    let metadata = &run.metadata_json;
    let exit_code = metadata.get("exit_code").and_then(|value| value.as_i64());
    let error = metadata
        .get("error")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    RunsEvidenceFailureSummary {
        failed: matches!(run.status.as_str(), "fail" | "failed" | "error" | "stale"),
        status: run.status.clone(),
        exit_code,
        error,
        failure: metadata.get("failure").cloned().unwrap_or(Value::Null),
        gate_failures: string_array(metadata.get("gate_failures")),
        hints: string_array(metadata.get("hints")),
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn evidence_links(artifacts: &[ArtifactRecord]) -> Vec<RunsEvidenceLink> {
    artifacts
        .iter()
        .filter_map(|artifact| {
            let target = artifact_public_url(artifact)?;
            let mut reference = EvidenceRef::new(&artifact.kind, &target, &artifact.kind);
            reference.artifact = Some(artifact_ref(artifact));
            Some(RunsEvidenceLink {
                kind: reference.kind.clone(),
                target: reference.target.clone(),
                label: reference.label.clone(),
                reference,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use homeboy::core::artifact_links::PUBLIC_ARTIFACT_BASE_URL_ENV;
    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
    use homeboy::test_support::with_isolated_home;
    use serde_json::Value;

    use super::*;

    struct XdgGuard(Option<String>);

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn unset(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
        NewRunRecord::builder(kind)
            .component_id(component_id)
            .command(format!("homeboy {kind} {component_id}"))
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .git_sha(Some("abc123".to_string()))
            .rig_id(rig_id)
            .metadata(metadata)
            .build()
    }

    #[test]
    fn evidence_command_reports_registry_artifacts_retention_and_failure_summary() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let _public_artifact_base = EnvGuard::unset(PUBLIC_ARTIFACT_BASE_URL_ENV);
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "exit_code": 1,
                        "error": "boom",
                        "gate_failures": ["p95_ms exceeded"],
                        "hints": ["inspect artifacts"],
                        "scenario_metrics": [{"scenario_id":"cold","metrics":{"p95_ms":42.0}}],
                        "resource_policy": {"hot_command":"bench"}
                    }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish run");
            let artifact_path = home.path().join("bench-results.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "bench_results", &artifact_path)
                .expect("record artifact");
            store
                .record_url_artifact(&run.id, "review", "https://example.test/evidence")
                .expect("record url");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            assert_eq!(output.command, "runs.evidence");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.run.kind, "bench");
            assert_eq!(output.artifact_index.count, 2);
            assert_eq!(output.artifact_index.file_count, 1);
            assert_eq!(output.artifact_index.url_count, 1);
            assert_eq!(output.artifact_index.missing_count, 0);
            let bench_results = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "bench_results")
                .expect("bench results artifact");
            assert!(!bench_results.public);
            assert_eq!(
                bench_results.relative_to.as_deref(),
                Some("homeboy observation artifact store")
            );
            let expected_fetch_command = format!(
                "homeboy runs artifact get {} {} -o <path>",
                run.id, bench_results.id
            );
            assert_eq!(
                bench_results.fetch_command.as_deref(),
                Some(expected_fetch_command.as_str())
            );
            assert_eq!(bench_results.reference.schema, "homeboy/artifact-ref/v1");
            assert_eq!(bench_results.reference.id, bench_results.id);
            let review = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "review")
                .expect("review artifact");
            assert!(review.public);
            assert_eq!(
                review.public_url.as_deref(),
                Some("https://example.test/evidence")
            );
            assert_eq!(output.evidence_links.len(), 1);
            assert_eq!(
                output.evidence_links[0].reference.schema,
                "homeboy/evidence-ref/v1"
            );
            assert_eq!(
                output.evidence_links[0].target,
                "https://example.test/evidence"
            );
            assert_eq!(output.retention.default_retention_days, 30);
            assert!(output
                .retention
                .artifact_root
                .contains("agent-readable-artifacts"));
            assert!(output
                .retention
                .cleanup_command
                .contains("cleanup-persisted --run-id"));
            assert!(output.failure.failed);
            assert_eq!(output.failure.exit_code, Some(1));
            assert_eq!(output.failure.gate_failures, vec!["p95_ms exceeded"]);
            assert_eq!(output.failure.hints, vec!["inspect artifacts"]);
            assert!(
                output.disk_budget.available_bytes.is_some()
                    || output.disk_budget.warning.is_some()
            );
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn evidence_failure_summary_does_not_mark_running_run_failed() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "trace",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "status": "running",
                        "phase": "waiting-for-child"
                    }),
                ))
                .expect("run");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            assert_eq!(output.run.status, "running");
            assert_eq!(output.failure.status, "running");
            assert!(!output.failure.failed);
        });
    }
}
