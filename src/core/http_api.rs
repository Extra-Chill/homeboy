//! Read-only local HTTP API contract.
//!
//! This module is intentionally transport-free: the daemon can hand it a
//! method/path pair and serialize the returned JSON without duplicating Homeboy
//! command behavior. Long-running analysis endpoints enqueue daemon-owned jobs
//! so HTTP requests can return immediately while clients poll job events.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::api_jobs::{ActiveRunnerJobSummary, JobStatus, JobStore};
use crate::core::error::{Error, Result};
use crate::core::observation::{
    running_status_note, FindingListFilter, ObservationStore, RunListFilter, RunRecord,
};
use crate::core::{component, git, paths, rig, runner, stack};

mod analysis_job_runner;
mod sandbox_tools;

pub use analysis_job_runner::{
    AnalysisJobRunOutput, AnalysisJobRunner, UnsupportedAnalysisJobRunner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpApiRequest {
    pub method: HttpMethod,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpApiResponse {
    pub status: u16,
    pub endpoint: String,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpEndpoint {
    Components,
    Component { id: String },
    ComponentStatus { id: String },
    ComponentChanges { id: String },
    Rigs,
    Rig { id: String },
    RigCheck { id: String },
    Stacks,
    Stack { id: String },
    StackStatus { id: String },
    Runs,
    Run { id: String },
    RunArtifacts { id: String },
    RunArtifactContent { id: String, artifact_id: String },
    RunFindings { id: String },
    AuditRuns,
    BenchRuns,
    Jobs,
    Job { id: String },
    JobEvents { id: String },
    JobCancel { id: String },
    JobReadyRun { kind: JobReadyRunKind },
    SandboxTools,
    SandboxTool { id: String },
    SandboxToolRun { id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub git_sha: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub homeboy_version: Option<String>,
    pub metadata: Value,
    pub artifacts: Vec<crate::core::observation::ArtifactRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobReadyRunKind {
    Audit,
    Lint,
    Test,
    Bench,
    Build,
    Review,
}

impl HttpEndpoint {
    fn name(&self) -> &'static str {
        match self {
            Self::Components => "components.list",
            Self::Component { .. } => "components.show",
            Self::ComponentStatus { .. } => "components.status",
            Self::ComponentChanges { .. } => "components.changes",
            Self::Rigs => "rigs.list",
            Self::Rig { .. } => "rigs.show",
            Self::RigCheck { .. } => "rigs.check",
            Self::Stacks => "stacks.list",
            Self::Stack { .. } => "stacks.show",
            Self::StackStatus { .. } => "stacks.status",
            Self::Runs => "runs.list",
            Self::Run { .. } => "runs.show",
            Self::RunArtifacts { .. } => "runs.artifacts",
            Self::RunArtifactContent { .. } => "runs.artifact.content",
            Self::RunFindings { .. } => "runs.findings",
            Self::AuditRuns => "audit.runs",
            Self::BenchRuns => "bench.runs",
            Self::Jobs => "jobs.list",
            Self::Job { .. } => "jobs.show",
            Self::JobEvents { .. } => "jobs.events",
            Self::JobCancel { .. } => "jobs.cancel",
            Self::JobReadyRun { .. } => "jobs.required",
            Self::SandboxTools => "tools.list",
            Self::SandboxTool { .. } => "tools.show",
            Self::SandboxToolRun { .. } => "tools.run",
        }
    }
}

/// Route an HTTP method/path pair to a Homeboy API endpoint.
pub fn route(method: HttpMethod, path: &str) -> Result<HttpEndpoint> {
    let segments = path_segments(path);
    let refs: Vec<&str> = segments.iter().map(String::as_str).collect();
    match (method, refs.as_slice()) {
        (HttpMethod::Get, ["components"]) => Ok(HttpEndpoint::Components),
        (HttpMethod::Get, ["components", id]) => Ok(HttpEndpoint::Component {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["components", id, "status"]) => Ok(HttpEndpoint::ComponentStatus {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["components", id, "changes"]) => Ok(HttpEndpoint::ComponentChanges {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["rigs"]) => Ok(HttpEndpoint::Rigs),
        (HttpMethod::Get, ["rigs", id]) => Ok(HttpEndpoint::Rig {
            id: (*id).to_string(),
        }),
        (HttpMethod::Post, ["rigs", id, "check"]) => Ok(HttpEndpoint::RigCheck {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["stacks"]) => Ok(HttpEndpoint::Stacks),
        (HttpMethod::Get, ["stacks", id]) => Ok(HttpEndpoint::Stack {
            id: (*id).to_string(),
        }),
        (HttpMethod::Post, ["stacks", id, "status"]) => Ok(HttpEndpoint::StackStatus {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["runs"]) => Ok(HttpEndpoint::Runs),
        (HttpMethod::Get, ["runs", id]) => Ok(HttpEndpoint::Run {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["runs", id, "artifacts"]) => Ok(HttpEndpoint::RunArtifacts {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["runs", id, "artifacts", artifact_id, "content"]) => {
            Ok(HttpEndpoint::RunArtifactContent {
                id: (*id).to_string(),
                artifact_id: (*artifact_id).to_string(),
            })
        }
        (HttpMethod::Get, ["runs", id, "findings"]) => Ok(HttpEndpoint::RunFindings {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["audit", "runs"]) => Ok(HttpEndpoint::AuditRuns),
        (HttpMethod::Get, ["bench", "runs"]) => Ok(HttpEndpoint::BenchRuns),
        (HttpMethod::Get, ["jobs"]) => Ok(HttpEndpoint::Jobs),
        (HttpMethod::Get, ["jobs", id]) => Ok(HttpEndpoint::Job {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["jobs", id, "events"]) => Ok(HttpEndpoint::JobEvents {
            id: (*id).to_string(),
        }),
        (HttpMethod::Post, ["jobs", id, "cancel"]) => Ok(HttpEndpoint::JobCancel {
            id: (*id).to_string(),
        }),
        (HttpMethod::Post, ["audit"]) => Ok(HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Audit,
        }),
        (HttpMethod::Post, ["lint"]) => Ok(HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Lint,
        }),
        (HttpMethod::Post, ["test"]) => Ok(HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Test,
        }),
        (HttpMethod::Post, ["bench"]) => Ok(HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Bench,
        }),
        (HttpMethod::Get, ["tools"]) => Ok(HttpEndpoint::SandboxTools),
        (HttpMethod::Get, ["tools", id]) => Ok(HttpEndpoint::SandboxTool {
            id: (*id).to_string(),
        }),
        (HttpMethod::Post, ["tools", id, "run"]) => Ok(HttpEndpoint::SandboxToolRun {
            id: (*id).to_string(),
        }),
        _ => Err(Error::validation_invalid_argument(
            "path",
            format!(
                "No read-only HTTP API route for {} {}",
                method_label(method),
                path
            ),
            Some(path.to_string()),
            Some(vec![
                "GET /components".to_string(),
                "GET /components/:id/status".to_string(),
                "GET /rigs".to_string(),
                "POST /rigs/:id/check".to_string(),
                "GET /stacks".to_string(),
                "POST /stacks/:id/status".to_string(),
                "GET /runs".to_string(),
                "GET /runs/:id".to_string(),
                "GET /runs/:id/artifacts".to_string(),
                "GET /runs/:id/artifacts/:artifact_id/content".to_string(),
                "GET /runs/:id/findings".to_string(),
                "GET /audit/runs".to_string(),
                "GET /bench/runs".to_string(),
                "GET /jobs".to_string(),
                "GET /jobs/:id".to_string(),
                "GET /jobs/:id/events".to_string(),
                "POST /jobs/:id/cancel".to_string(),
                "GET /tools".to_string(),
                "GET /tools/:id".to_string(),
                "POST /tools/:id/run".to_string(),
            ]),
        )),
    }
}

/// Execute a routed read-only API request through existing Homeboy core code.
pub fn handle(request: HttpApiRequest) -> Result<HttpApiResponse> {
    handle_with_jobs(request, &JobStore::default())
}

/// Execute a routed HTTP API request against the daemon-owned in-memory job store.
pub fn handle_with_jobs(request: HttpApiRequest, job_store: &JobStore) -> Result<HttpApiResponse> {
    handle_with_jobs_and_runner(request, job_store, UnsupportedAnalysisJobRunner)
}

/// Execute a routed HTTP API request with an injected analysis job runner.
pub fn handle_with_jobs_and_runner<R>(
    request: HttpApiRequest,
    job_store: &JobStore,
    analysis_runner: R,
) -> Result<HttpApiResponse>
where
    R: AnalysisJobRunner,
{
    let endpoint = route(request.method, &request.path)?;
    let body = match &endpoint {
        HttpEndpoint::Components => json!({
            "command": "api.components.list",
            "components": component::inventory()?,
        }),
        HttpEndpoint::Component { id } => json!({
            "command": "api.components.show",
            "component": component::resolve_effective(Some(id), None, None)?,
        }),
        HttpEndpoint::ComponentStatus { id } => json!({
            "command": "api.components.status",
            "status": git::status(Some(id))?,
        }),
        HttpEndpoint::ComponentChanges { id } => json!({
            "command": "api.components.changes",
            "changes": git::changes(Some(id), None, false)?,
        }),
        HttpEndpoint::Rigs => json!({
            "command": "api.rigs.list",
            "rigs": rig::list()?,
        }),
        HttpEndpoint::Rig { id } => json!({
            "command": "api.rigs.show",
            "rig": rig::load(id)?,
        }),
        HttpEndpoint::RigCheck { id } => {
            let rig = rig::load(id)?;
            json!({
                "command": "api.rigs.check",
                "report": rig::run_check(&rig)?,
            })
        }
        HttpEndpoint::Stacks => json!({
            "command": "api.stacks.list",
            "stacks": stack::list()?,
        }),
        HttpEndpoint::Stack { id } => json!({
            "command": "api.stacks.show",
            "stack": stack::load(id)?,
        }),
        HttpEndpoint::StackStatus { id } => {
            let spec = stack::load(id)?;
            json!({
                "command": "api.stacks.status",
                "report": stack::status(&spec)?,
            })
        }
        HttpEndpoint::Runs => json!({
            "command": "api.runs.list",
            "runs": list_runs(&request.path, None, job_store)?,
            "active_runner_jobs": active_runner_jobs_for_path(&request.path, job_store),
        }),
        HttpEndpoint::Run { id } => json!({
            "command": "api.runs.show",
            "run": show_run(id)?,
        }),
        HttpEndpoint::RunArtifacts { id } => {
            let store = ObservationStore::open_initialized()?;
            require_run(&store, id)?;
            json!({
                "command": "api.runs.artifacts",
                "run_id": id,
                "artifacts": store.list_artifacts(id)?,
            })
        }
        HttpEndpoint::RunArtifactContent { id, artifact_id } => artifact_content(id, artifact_id)?,
        HttpEndpoint::RunFindings { id } => {
            let store = ObservationStore::open_initialized()?;
            require_run(&store, id)?;
            json!({
                "command": "api.runs.findings",
                "run_id": id,
                "findings": store.list_findings(FindingListFilter {
                    run_id: Some(id.clone()),
                    tool: query_value(&request.path, "tool"),
                    file: query_value(&request.path, "file"),
                    fingerprint: query_value(&request.path, "fingerprint"),
                    limit: query_value(&request.path, "limit")
                        .and_then(|value| value.parse::<i64>().ok())
                        .map(|limit| limit.clamp(1, 1000)),
                })?,
            })
        }
        HttpEndpoint::AuditRuns => json!({
            "command": "api.audit.runs",
            "runs": list_runs(&request.path, Some("audit"), job_store)?,
        }),
        HttpEndpoint::BenchRuns => json!({
            "command": "api.bench.runs",
            "runs": list_runs(&request.path, Some("bench"), job_store)?,
        }),
        HttpEndpoint::Jobs => json!({
            "command": "api.jobs.list",
            "jobs": job_store.list(),
            "active_runner_jobs": job_store.active_runner_jobs(),
        }),
        HttpEndpoint::Job { id } => json!({
            "command": "api.jobs.show",
            "job": job_store.get(parse_job_id(id)?)?,
        }),
        HttpEndpoint::JobEvents { id } => json!({
            "command": "api.jobs.events",
            "job_id": id,
            "events": job_store.events(parse_job_id(id)?)?,
        }),
        HttpEndpoint::JobCancel { id } => json!({
            "command": "api.jobs.cancel",
            "job": job_store.cancel(parse_job_id(id)?, "cancel requested via HTTP API")?,
        }),
        HttpEndpoint::JobReadyRun { kind } => {
            enqueue_analysis_job(job_store, *kind, request.body, analysis_runner)?
        }
        HttpEndpoint::SandboxTools => json!({
            "command": "api.tools.list",
            "tools": sandbox_tools::all(),
        }),
        HttpEndpoint::SandboxTool { id } => json!({
            "command": "api.tools.show",
            "tool": sandbox_tools::get(id)?,
        }),
        HttpEndpoint::SandboxToolRun { id } => {
            enqueue_sandbox_tool_job(job_store, id, request.body, analysis_runner)?
        }
    };

    Ok(HttpApiResponse {
        status: 200,
        endpoint: endpoint.name().to_string(),
        body,
    })
}

fn enqueue_sandbox_tool_job(
    job_store: &JobStore,
    id: &str,
    body: Option<Value>,
    analysis_runner: impl AnalysisJobRunner,
) -> Result<Value> {
    let tool = sandbox_tools::get(id)?;
    let kind = sandbox_tools::kind(tool.id)?;
    let mut response = enqueue_analysis_job(job_store, kind, body, analysis_runner)?;
    if let Value::Object(ref mut fields) = response {
        fields.insert("command".to_string(), json!("api.tools.run.enqueue"));
        fields.insert(
            "tool".to_string(),
            serde_json::to_value(tool).unwrap_or(Value::Null),
        );
    }
    Ok(response)
}

fn artifact_content(run_id: &str, artifact_id: &str) -> Result<Value> {
    let store = ObservationStore::open_initialized()?;
    require_run(&store, run_id)?;
    let Some(artifact) = store.get_artifact(artifact_id)? else {
        return artifact_store_content(run_id, artifact_id);
    };
    if artifact.run_id != run_id {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "artifact does not belong to requested run",
            Some(artifact_id.to_string()),
            None,
        ));
    }
    if artifact.artifact_type != "file" {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a downloadable file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id),
            None,
        ));
    }
    let path = std::path::PathBuf::from(&artifact.path);
    let content = std::fs::read(&path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read recorded artifact {}", path.display())),
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&artifact.id);
    Ok(json!({
        "command": "api.runs.artifact.content",
        "run_id": run_id,
        "artifact_id": artifact.id,
        "filename": filename,
        "mime": artifact.mime,
        "size_bytes": artifact.size_bytes,
        "sha256": artifact.sha256,
        "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
    }))
}

fn artifact_store_content(run_id: &str, artifact_id: &str) -> Result<Value> {
    let decoded_artifact_id = crate::core::execution_contract::decode_uri_component(artifact_id);
    let locator = runner::artifact_store_locator_from_runner_artifact_id(&decoded_artifact_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("artifact record not found: {artifact_id}"),
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    let path = safe_artifact_store_path(&locator)?;
    let content = std::fs::read(&path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read artifact-store locator {}", path.display())),
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(artifact_id);
    let size_bytes = i64::try_from(content.len()).ok();
    let sha256 = crate::core::artifact_metadata::sha256_file(&path).ok();
    Ok(json!({
        "command": "api.runs.artifact.content",
        "run_id": run_id,
        "artifact_id": artifact_id,
        "filename": filename,
        "mime": crate::core::artifact_metadata::content_type_from_path(&path),
        "size_bytes": size_bytes,
        "sha256": sha256,
        "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
    }))
}

fn safe_artifact_store_path(locator: &str) -> Result<std::path::PathBuf> {
    let locator_path = std::path::PathBuf::from(locator);
    if locator_path.is_absolute()
        || locator_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "artifact-store locator must stay under the artifact root",
            Some(locator.to_string()),
            None,
        ));
    }
    Ok(paths::artifact_root()?.join(locator_path))
}

fn path_segments(path: &str) -> Vec<String> {
    path.split('?')
        .next()
        .unwrap_or(path)
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

fn list_runs(
    path: &str,
    kind_override: Option<&str>,
    job_store: &JobStore,
) -> Result<Vec<RunSummary>> {
    let store = ObservationStore::open_initialized()?;
    let filter = RunListFilter {
        kind: kind_override
            .map(str::to_string)
            .or_else(|| query_value(path, "kind")),
        component_id: query_value(path, "component").or_else(|| query_value(path, "component_id")),
        status: query_value(path, "status"),
        rig_id: query_value(path, "rig").or_else(|| query_value(path, "rig_id")),
        limit: query_value(path, "limit")
            .and_then(|value| value.parse::<i64>().ok())
            .map(|limit| limit.clamp(1, 1000)),
    };

    let mut runs: Vec<RunSummary> = store
        .list_runs(filter)?
        .into_iter()
        .map(run_summary)
        .collect();

    if kind_override.is_none() {
        runs.extend(
            active_runner_jobs_for_path(path, job_store)
                .into_iter()
                .map(active_runner_job_run_summary),
        );
    }

    Ok(runs)
}

fn active_runner_jobs_for_path(path: &str, job_store: &JobStore) -> Vec<ActiveRunnerJobSummary> {
    let status = query_value(path, "status");
    let limit = query_value(path, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|limit| limit.clamp(1, 1000));
    let mut jobs: Vec<_> = job_store
        .active_runner_jobs()
        .into_iter()
        .filter(|job| match status.as_deref() {
            Some(status) => status == active_job_status_label(job.status),
            None => true,
        })
        .collect();
    if let Some(limit) = limit {
        jobs.truncate(limit);
    }
    jobs
}

fn active_runner_job_run_summary(job: ActiveRunnerJobSummary) -> RunSummary {
    RunSummary {
        id: job
            .durable_run_id
            .clone()
            .unwrap_or_else(|| format!("runner-job-{}", job.job_id)),
        kind: "lab-runner-job".to_string(),
        status: active_job_status_label(job.status).to_string(),
        started_at: ms_to_rfc3339(job.started_at_ms),
        finished_at: None,
        component_id: None,
        rig_id: None,
        git_sha: None,
        command: Some(format!(
            "{} [runner={}, job={}, durable_run={}, elapsed_ms={}, active_child_count={}, active_cell_count={}]",
            job.command,
            job.runner_id,
            job.job_id,
            job.durable_run_id.as_deref().unwrap_or("unknown"),
            job.elapsed_ms,
            job.active_child_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            job.active_cell_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )),
        cwd: job.cwd,
        status_note: Some(format!(
            "active Lab runner job: runner={} job={} durable_run={} elapsed_ms={} active_child_count={} active_cell_count={}",
            job.runner_id,
            job.job_id,
            job.durable_run_id.as_deref().unwrap_or("unknown"),
            job.elapsed_ms,
            job.active_child_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            job.active_cell_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )),
    }
}

fn active_job_status_label(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "pass",
        JobStatus::Failed => "fail",
        JobStatus::Cancelled => "cancelled",
    }
}

fn ms_to_rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

fn show_run(run_id: &str) -> Result<RunDetail> {
    let store = ObservationStore::open_initialized()?;
    let run = require_run(&store, run_id)?;
    Ok(RunDetail {
        summary: run_summary(run.clone()),
        homeboy_version: run.homeboy_version,
        metadata: run.metadata_json,
        artifacts: store.list_artifacts(run_id)?,
    })
}

fn require_run(store: &ObservationStore, run_id: &str) -> Result<RunRecord> {
    store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })
}

fn parse_job_id(job_id: &str) -> Result<Uuid> {
    Uuid::parse_str(job_id).map_err(|_| {
        Error::validation_invalid_argument(
            "job_id",
            format!("invalid job id: {job_id}"),
            Some(job_id.to_string()),
            None,
        )
    })
}

fn enqueue_analysis_job(
    job_store: &JobStore,
    kind: JobReadyRunKind,
    body: Option<Value>,
    analysis_runner: impl AnalysisJobRunner,
) -> Result<Value> {
    let request = AnalysisJobRequest::from_body(kind, body)?;
    let argv = request.argv();
    let operation = format!("analysis.{}", job_ready_slug(kind));
    let request_summary = request.summary();
    let command_label = request.command_label();
    let runner = job_store.run_background(operation, move |job| {
        job.progress(json!({
            "phase": "started",
            "command": command_label,
            "job_id": job.job_id(),
        }))?;

        let output = analysis_runner.run_analysis_job(argv)?;
        job.progress(json!({
            "phase": "finished",
            "exit_code": output.exit_code,
        }))?;
        Ok(json!({
            "command": command_label,
            "exit_code": output.exit_code,
            "output": output.output,
        }))
    });
    let job = job_store.get(runner.job_id)?;

    Ok(json!({
        "command": format!("api.{}.enqueue", job_ready_slug(kind)),
        "job": job,
        "poll": {
            "job": format!("/jobs/{}", runner.job_id),
            "events": format!("/jobs/{}/events", runner.job_id),
        },
        "request": request_summary,
    }))
}

#[derive(Debug, Clone)]
struct AnalysisJobRequest {
    kind: JobReadyRunKind,
    args: Vec<String>,
    summary: Value,
}

impl AnalysisJobRequest {
    fn from_body(kind: JobReadyRunKind, body: Option<Value>) -> Result<Self> {
        let mut parser = AnalysisBodyParser::new(body)?;
        let mut args = vec![job_ready_slug(kind).to_string()];

        match kind {
            JobReadyRunKind::Audit => {
                parser.push_optional_string("component", &mut args)?;
                parser.push_optional_flag_value("path", "--path", &mut args)?;
                parser.push_bool_flag("json_summary", "--json-summary", &mut args)?;
                parser.push_bool_flag("conventions", "--conventions", &mut args)?;
                parser.push_string_array("only", "--only", &mut args)?;
                parser.push_string_array("exclude", "--exclude", &mut args)?;
                parser.push_optional_flag_value("changed_since", "--changed-since", &mut args)?;
                parser.push_bool_flag("fixability", "--fixability", &mut args)?;
            }
            JobReadyRunKind::Lint => {
                parser.push_optional_string("component", &mut args)?;
                parser.push_optional_flag_value("path", "--path", &mut args)?;
                parser.push_bool_flag("json_summary", "--json-summary", &mut args)?;
                parser.push_bool_flag("summary", "--summary", &mut args)?;
                parser.push_optional_flag_value("file", "--file", &mut args)?;
                parser.push_optional_flag_value("glob", "--glob", &mut args)?;
                parser.push_bool_flag("changed_only", "--changed-only", &mut args)?;
                parser.push_optional_flag_value("changed_since", "--changed-since", &mut args)?;
                parser.push_bool_flag("errors_only", "--errors-only", &mut args)?;
                parser.push_optional_flag_value("sniffs", "--sniffs", &mut args)?;
                parser.push_optional_flag_value("exclude_sniffs", "--exclude-sniffs", &mut args)?;
                parser.push_optional_flag_value("category", "--category", &mut args)?;
                parser.reject_present("fix", "POST /lint jobs do not expose mutating --fix")?;
            }
            JobReadyRunKind::Test => {
                parser.push_optional_string("component", &mut args)?;
                parser.push_optional_flag_value("path", "--path", &mut args)?;
                parser.push_bool_flag("json_summary", "--json-summary", &mut args)?;
                parser.push_bool_flag("skip_lint", "--skip-lint", &mut args)?;
                parser.push_bool_flag("coverage", "--coverage", &mut args)?;
                parser.push_optional_number("coverage_min", "--coverage-min", &mut args)?;
                parser.push_bool_flag("analyze", "--analyze", &mut args)?;
                parser.push_bool_flag("drift", "--drift", &mut args)?;
                parser.push_optional_flag_value("since", "--since", &mut args)?;
                parser.push_optional_flag_value("changed_since", "--changed-since", &mut args)?;
                parser.push_passthrough_args(&mut args)?;
                parser.reject_present("write", "POST /test jobs do not expose mutating --write")?;
            }
            JobReadyRunKind::Bench => {
                parser.push_optional_string("component", &mut args)?;
                parser.push_optional_flag_value("path", "--path", &mut args)?;
                parser.push_bool_flag("json_summary", "--json-summary", &mut args)?;
                parser.push_optional_u64("iterations", "--iterations", &mut args)?;
                parser.push_optional_u64("warmup", "--warmup", &mut args)?;
                parser.push_optional_u64("runs", "--runs", &mut args)?;
                parser.push_optional_u32("concurrency", "--concurrency", &mut args)?;
                parser.push_string_array("rig", "--rig", &mut args)?;
                parser.push_string_array("scenario", "--scenario", &mut args)?;
                parser.push_optional_flag_value("profile", "--profile", &mut args)?;
                parser.push_optional_number(
                    "regression_threshold",
                    "--regression-threshold",
                    &mut args,
                )?;
                parser.push_bool_flag(
                    "ignore_default_baseline",
                    "--ignore-default-baseline",
                    &mut args,
                )?;
                parser.push_passthrough_args(&mut args)?;
            }
            JobReadyRunKind::Build => {
                parser.push_optional_string("component", &mut args)?;
                parser.push_string_array_values("component_ids", &mut args)?;
                parser.push_optional_flag_value("path", "--path", &mut args)?;
                parser.push_bool_flag("all", "--all", &mut args)?;
                parser.reject_present("json", "sandbox build jobs do not expose --json")?;
            }
            JobReadyRunKind::Review => {
                parser.push_optional_string("component", &mut args)?;
                parser.push_optional_flag_value("path", "--path", &mut args)?;
                parser.push_optional_flag_value("changed_since", "--changed-since", &mut args)?;
                parser.push_bool_flag("changed_only", "--changed-only", &mut args)?;
                parser.push_bool_flag("summary", "--summary", &mut args)?;
                parser.push_optional_flag_value("ci_profile", "--ci-profile", &mut args)?;
                parser.reject_present("report", "sandbox review jobs keep JSON output")?;
                parser.reject_present("banner", "sandbox review jobs do not expose --banner")?;
            }
        }

        parser.reject_present(
            "baseline",
            "analysis jobs do not expose mutating --baseline",
        )?;
        parser.reject_present("ratchet", "analysis jobs do not expose mutating --ratchet")?;
        parser.reject_present(
            "shared_state",
            "POST /bench jobs do not expose --shared-state",
        )?;
        parser.reject_unknown()?;

        Ok(Self {
            kind,
            summary: parser.summary(),
            args,
        })
    }

    fn argv(&self) -> Vec<String> {
        let mut argv = vec!["homeboy".to_string()];
        argv.extend(self.args.clone());
        argv
    }

    fn command_label(&self) -> String {
        format!("homeboy {}", self.args.join(" "))
    }

    fn summary(&self) -> Value {
        json!({
            "kind": job_ready_slug(self.kind),
            "args": self.args,
            "body": self.summary,
        })
    }
}

struct AnalysisBodyParser {
    fields: serde_json::Map<String, Value>,
    consumed: Vec<String>,
}

impl AnalysisBodyParser {
    fn new(body: Option<Value>) -> Result<Self> {
        match body.unwrap_or_else(|| json!({})) {
            Value::Object(fields) => Ok(Self {
                fields,
                consumed: Vec::new(),
            }),
            other => Err(Error::validation_invalid_argument(
                "body",
                "request body must be a JSON object",
                Some(other.to_string()),
                None,
            )),
        }
    }

    fn push_optional_string(&mut self, key: &str, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take_string(key)? {
            args.push(value);
        }
        Ok(())
    }

    fn push_optional_flag_value(
        &mut self,
        key: &str,
        flag: &str,
        args: &mut Vec<String>,
    ) -> Result<()> {
        if let Some(value) = self.take_string(key)? {
            args.push(flag.to_string());
            args.push(value);
        }
        Ok(())
    }

    fn push_bool_flag(&mut self, key: &str, flag: &str, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take(key) {
            match value {
                Value::Bool(true) => args.push(flag.to_string()),
                Value::Bool(false) | Value::Null => {}
                other => return Err(invalid_body_type(key, "boolean", &other)),
            }
        }
        Ok(())
    }

    fn push_string_array(&mut self, key: &str, flag: &str, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take(key) {
            match value {
                Value::Array(values) => {
                    for value in values {
                        let Some(value) = value.as_str() else {
                            return Err(invalid_body_type(key, "array of strings", &value));
                        };
                        args.push(flag.to_string());
                        args.push(value.to_string());
                    }
                }
                Value::String(value) => {
                    args.push(flag.to_string());
                    args.push(value);
                }
                Value::Null => {}
                other => return Err(invalid_body_type(key, "string or array of strings", &other)),
            }
        }
        Ok(())
    }

    fn push_string_array_values(&mut self, key: &str, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take(key) {
            match value {
                Value::Array(values) => {
                    for value in values {
                        let Some(value) = value.as_str() else {
                            return Err(invalid_body_type(key, "array of strings", &value));
                        };
                        args.push(value.to_string());
                    }
                }
                Value::String(value) => args.push(value),
                Value::Null => {}
                other => return Err(invalid_body_type(key, "string or array of strings", &other)),
            }
        }
        Ok(())
    }

    fn push_optional_number(
        &mut self,
        key: &str,
        flag: &str,
        args: &mut Vec<String>,
    ) -> Result<()> {
        if let Some(value) = self.take(key) {
            match value {
                Value::Number(number) => {
                    args.push(flag.to_string());
                    args.push(number.to_string());
                }
                Value::Null => {}
                other => return Err(invalid_body_type(key, "number", &other)),
            }
        }
        Ok(())
    }

    fn push_optional_u64(&mut self, key: &str, flag: &str, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take(key) {
            match value {
                Value::Number(number) if number.as_u64().is_some() => {
                    args.push(flag.to_string());
                    args.push(number.to_string());
                }
                Value::Null => {}
                other => return Err(invalid_body_type(key, "unsigned integer", &other)),
            }
        }
        Ok(())
    }

    fn push_optional_u32(&mut self, key: &str, flag: &str, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take(key) {
            match value {
                Value::Number(number) => {
                    let Some(parsed) = number.as_u64().and_then(|value| u32::try_from(value).ok())
                    else {
                        return Err(invalid_body_type(key, "u32", &Value::Number(number)));
                    };
                    args.push(flag.to_string());
                    args.push(parsed.to_string());
                }
                Value::Null => {}
                other => return Err(invalid_body_type(key, "u32", &other)),
            }
        }
        Ok(())
    }

    fn push_passthrough_args(&mut self, args: &mut Vec<String>) -> Result<()> {
        if let Some(value) = self.take("args") {
            match value {
                Value::Array(values) if values.is_empty() => {}
                Value::Array(values) => {
                    args.push("--".to_string());
                    for value in values {
                        let Some(value) = value.as_str() else {
                            return Err(invalid_body_type("args", "array of strings", &value));
                        };
                        args.push(value.to_string());
                    }
                }
                Value::Null => {}
                other => return Err(invalid_body_type("args", "array of strings", &other)),
            }
        }
        Ok(())
    }

    fn reject_present(&mut self, key: &str, message: &str) -> Result<()> {
        if self.take(key).is_some() {
            return Err(Error::validation_invalid_argument(
                key,
                message,
                Some(key.to_string()),
                None,
            ));
        }
        Ok(())
    }

    fn reject_unknown(&self) -> Result<()> {
        if self.fields.is_empty() {
            return Ok(());
        }
        let mut unknown: Vec<String> = self.fields.keys().cloned().collect();
        unknown.sort();
        Err(Error::validation_invalid_argument(
            "body",
            format!(
                "unsupported analysis job body field(s): {}",
                unknown.join(", ")
            ),
            Some(unknown.join(",")),
            None,
        ))
    }

    fn summary(&self) -> Value {
        json!({ "accepted_fields": self.consumed })
    }

    fn take_string(&mut self, key: &str) -> Result<Option<String>> {
        let Some(value) = self.take(key) else {
            return Ok(None);
        };
        match value {
            Value::String(value) => Ok(Some(value)),
            Value::Null => Ok(None),
            other => Err(invalid_body_type(key, "string", &other)),
        }
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        let value = self.fields.remove(key)?;
        self.consumed.push(key.to_string());
        Some(value)
    }
}

fn invalid_body_type(key: &str, expected: &str, value: &Value) -> Error {
    Error::validation_invalid_argument(
        key,
        format!("{key} must be {expected}"),
        Some(value.to_string()),
        None,
    )
}

fn run_summary(run: RunRecord) -> RunSummary {
    let status_note = running_status_note(&run);
    RunSummary {
        id: run.id,
        kind: run.kind,
        status: run.status,
        started_at: run.started_at,
        finished_at: run.finished_at,
        component_id: run.component_id,
        rig_id: run.rig_id,
        git_sha: run.git_sha,
        command: run.command,
        cwd: run.cwd,
        status_note,
    }
}

fn query_value(path: &str, key: &str) -> Option<String> {
    path.split_once('?')?.1.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (name == key && !value.is_empty()).then(|| value.to_string())
    })
}

fn method_label(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
    }
}

fn job_ready_slug(kind: JobReadyRunKind) -> &'static str {
    match kind {
        JobReadyRunKind::Audit => "audit",
        JobReadyRunKind::Lint => "lint",
        JobReadyRunKind::Test => "test",
        JobReadyRunKind::Bench => "bench",
        JobReadyRunKind::Build => "build",
        JobReadyRunKind::Review => "review",
    }
}

#[cfg(test)]
#[path = "../../tests/core/http_api_test.rs"]
mod http_api_test;
