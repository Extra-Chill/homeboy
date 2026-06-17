use clap::Args;
use serde::Serialize;

use homeboy::core::observation::{
    ArtifactRecord, ObservationStore, RunEvidenceCommands, RunListFilter, RunRecord,
};

use super::common::since_threshold;
use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Debug)]
pub struct RunsRefsArgs {
    /// Component ID.
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Run kind: bench, rig, trace, gh-actions, etc.
    #[arg(long)]
    pub kind: Option<String>,
    /// Rig ID.
    #[arg(long)]
    pub rig: Option<String>,
    /// Run status.
    #[arg(long)]
    pub status: Option<String>,
    /// Restrict to runs started within this duration (e.g. 24h, 7d).
    #[arg(long)]
    pub since: Option<String>,
    /// Maximum runs to inspect.
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
    /// Restrict artifact refs to these artifact kinds. Repeatable.
    #[arg(long = "artifact-kind")]
    pub artifact_kinds: Vec<String>,
    /// Treat these artifact kinds as aggregate refs in addition to the default
    /// schema-blind aggregate detector. Repeatable.
    #[arg(long = "aggregate-artifact-kind")]
    pub aggregate_artifact_kinds: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsRefsOutput {
    pub command: &'static str,
    pub filters: RunsRefsFilters,
    pub run_count: usize,
    pub artifact_count: usize,
    pub aggregate_artifact_count: usize,
    pub runs: Vec<RunRef>,
    pub artifacts: Vec<ArtifactRef>,
    pub aggregate_artifacts: Vec<ArtifactRef>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsRefsFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub limit: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifact_kinds: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aggregate_artifact_kinds: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunRef {
    pub run_id: String,
    pub ref_id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub git_sha: Option<String>,
    #[serde(flatten)]
    pub evidence_commands: RunEvidenceCommands,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ArtifactRef {
    pub run_id: String,
    pub artifact_id: String,
    pub ref_id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    pub url: Option<String>,
    pub mime: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub get_command: String,
}

pub fn runs_refs(args: RunsRefsArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let filter = RunListFilter {
        kind: args.kind.clone(),
        component_id: args.component_id.clone(),
        status: args.status.clone(),
        rig_id: args.rig.clone(),
        limit: Some(args.limit.clamp(1, 5000)),
    };
    let runs = if let Some(raw_since) = args.since.as_deref() {
        let threshold = since_threshold(raw_since)?;
        store
            .list_runs_started_since(&threshold)?
            .into_iter()
            .filter(|run| run_matches_filter(run, &filter))
            .take(args.limit.clamp(1, 5000) as usize)
            .collect::<Vec<_>>()
    } else {
        store.list_runs(filter.clone())?
    };

    let mut run_refs = Vec::with_capacity(runs.len());
    let mut artifact_refs = Vec::new();
    let mut aggregate_refs = Vec::new();
    for run in runs {
        run_refs.push(run_ref(&run));
        for artifact in store.list_artifacts(&run.id)? {
            if !args.artifact_kinds.is_empty() && !args.artifact_kinds.contains(&artifact.kind) {
                continue;
            }
            let artifact_ref = artifact_ref(&artifact);
            if is_aggregate_artifact(&artifact, &args.aggregate_artifact_kinds) {
                aggregate_refs.push(artifact_ref.clone());
            }
            artifact_refs.push(artifact_ref);
        }
    }

    Ok((
        RunsOutput::Refs(RunsRefsOutput {
            command: "runs.refs",
            filters: RunsRefsFilters {
                component_id: args.component_id,
                kind: args.kind,
                rig: args.rig,
                status: args.status,
                since: args.since,
                limit: args.limit,
                artifact_kinds: args.artifact_kinds,
                aggregate_artifact_kinds: args.aggregate_artifact_kinds,
            },
            run_count: run_refs.len(),
            artifact_count: artifact_refs.len(),
            aggregate_artifact_count: aggregate_refs.len(),
            runs: run_refs,
            artifacts: artifact_refs,
            aggregate_artifacts: aggregate_refs,
        }),
        0,
    ))
}

fn run_matches_filter(run: &RunRecord, filter: &RunListFilter) -> bool {
    filter.kind.as_deref().map_or(true, |kind| run.kind == kind)
        && filter.component_id.as_deref().map_or(true, |component| {
            run.component_id.as_deref() == Some(component)
        })
        && filter
            .status
            .as_deref()
            .map_or(true, |status| run.status == status)
        && filter
            .rig_id
            .as_deref()
            .map_or(true, |rig| run.rig_id.as_deref() == Some(rig))
}

fn run_ref(run: &RunRecord) -> RunRef {
    RunRef {
        run_id: run.id.clone(),
        ref_id: format!("homeboy://run/{}", run.id),
        kind: run.kind.clone(),
        status: run.status.clone(),
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        component_id: run.component_id.clone(),
        rig_id: run.rig_id.clone(),
        git_sha: run.git_sha.clone(),
        evidence_commands: RunEvidenceCommands::for_run_id(&shell_arg(&run.id)),
    }
}

fn artifact_ref(artifact: &ArtifactRecord) -> ArtifactRef {
    ArtifactRef {
        run_id: artifact.run_id.clone(),
        artifact_id: artifact.id.clone(),
        ref_id: format!("homeboy://run/{}/artifact/{}", artifact.run_id, artifact.id),
        kind: artifact.kind.clone(),
        artifact_type: artifact.artifact_type.clone(),
        path: artifact.path.clone(),
        url: artifact
            .url
            .clone()
            .or_else(|| (artifact.artifact_type == "url").then(|| artifact.path.clone())),
        mime: artifact.mime.clone(),
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256.clone(),
        get_command: format!(
            "homeboy runs artifact get {} {}",
            shell_arg(&artifact.run_id),
            shell_arg(&artifact.id)
        ),
    }
}

fn is_aggregate_artifact(artifact: &ArtifactRecord, extra_kinds: &[String]) -> bool {
    extra_kinds.contains(&artifact.kind)
        || contains_aggregate_token(&artifact.kind)
        || contains_aggregate_token(&artifact.id)
        || contains_aggregate_token(&artifact.path)
}

fn contains_aggregate_token(value: &str) -> bool {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case("aggregate"))
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::observation::{NewRunRecord, RunStatus};
    use homeboy::test_support::with_isolated_home;

    #[test]
    fn refs_emit_run_artifact_and_aggregate_refs() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("bench")
                        .component_id("homeboy")
                        .rig_id("matrix-rig")
                        .metadata(serde_json::json!({ "matrix": { "php": "8.3" } }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish");
            let aggregate_path = home.path().join("matrix.aggregate.json");
            std::fs::write(&aggregate_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "trace_aggregate", &aggregate_path)
                .expect("record artifact");

            let (output, _) = runs_refs(RunsRefsArgs {
                component_id: Some("homeboy".to_string()),
                kind: Some("bench".to_string()),
                rig: Some("matrix-rig".to_string()),
                status: Some("pass".to_string()),
                since: None,
                limit: 20,
                artifact_kinds: Vec::new(),
                aggregate_artifact_kinds: Vec::new(),
            })
            .expect("refs");

            let RunsOutput::Refs(output) = output else {
                panic!("expected refs output");
            };
            assert_eq!(output.run_count, 1);
            assert_eq!(output.artifact_count, 1);
            assert_eq!(output.aggregate_artifact_count, 1);
            assert_eq!(output.runs[0].ref_id, format!("homeboy://run/{}", run.id));
            assert_eq!(output.artifacts[0].run_id, run.id);
            assert_eq!(
                output.aggregate_artifacts[0].artifact_id,
                output.artifacts[0].artifact_id
            );
        });
    }
}
