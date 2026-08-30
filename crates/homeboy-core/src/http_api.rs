//! Read-only local HTTP API contract.
//!
//! This module is intentionally transport-free: the daemon can hand it a
//! method/path pair and serialize the returned JSON without duplicating Homeboy
//! command behavior. Long-running analysis endpoints enqueue daemon-owned jobs
//! so HTTP requests can return immediately while clients poll job events.

use base64::Engine;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::api_jobs::{self, ActiveRunnerJobSummary, JobStore, RunnerJobProjectionCancelRequest};
use crate::error::{Error, Result};
use crate::observation::{
    run_owner_pid, running_status_note, ArtifactListFilter, FindingListFilter, ObservationStore,
    RunListFilter, RunRecord, RunStatus, MAX_RUN_PAGE_LIMIT,
    OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES,
};
use crate::{activity, component, git};

mod analysis_job_runner;
mod sandbox_tools;
mod types;

pub use analysis_job_runner::{
    AnalysisJobRunOutput, AnalysisJobRunner, UnsupportedAnalysisJobRunner,
};
pub use types::{
    HttpApiRequest, HttpApiResponse, HttpEndpoint, HttpMethod, JobReadyRunKind, RunDetail,
    RunSummary,
};

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
        (HttpMethod::Get, ["runs", id, "artifacts", artifact_id]) => {
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
        (HttpMethod::Get, ["activity"]) => Ok(HttpEndpoint::Activity),
        (HttpMethod::Get, ["activity", id]) => Ok(HttpEndpoint::ActivityItem {
            id: (*id).to_string(),
        }),
        (HttpMethod::Get, ["v1", "control-plane", "capabilities"]) => {
            Ok(HttpEndpoint::ControlPlaneCapabilities)
        }
        (HttpMethod::Get, ["v1", "control-plane", "runs", id]) => {
            Ok(HttpEndpoint::ControlPlaneRun {
                id: (*id).to_string(),
            })
        }
        (HttpMethod::Get, ["v1", "control-plane", "runs", id, "events"]) => {
            let cursor = query_value(path, "cursor")
                .map(homeboy_control_plane_contract::EventCursor::new)
                .transpose()
                .map_err(|error| {
                    Error::validation_invalid_argument("cursor", error.to_string(), None, None)
                })?;
            Ok(HttpEndpoint::ControlPlaneRunEvents {
                id: (*id).to_string(),
                cursor,
            })
        }
        (HttpMethod::Post, ["v1", "control-plane", "runs", id, "actions"]) => {
            Ok(HttpEndpoint::ControlPlaneRunActions {
                id: (*id).to_string(),
            })
        }
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
        (HttpMethod::Post, ["jobs", id, "cancel-projection"]) => {
            Ok(HttpEndpoint::JobProjectionCancel {
                id: (*id).to_string(),
            })
        }
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
                "GET /runs/:id/artifacts/:artifact_id".to_string(),
                "GET /runs/:id/artifacts/:artifact_id/content".to_string(),
                "GET /runs/:id/findings".to_string(),
                "GET /audit/runs".to_string(),
                "GET /bench/runs".to_string(),
                "GET /activity".to_string(),
                "GET /activity/:id".to_string(),
                "GET /v1/control-plane/capabilities".to_string(),
                "GET /v1/control-plane/runs/:id".to_string(),
                "GET /v1/control-plane/runs/:id/events".to_string(),
                "POST /v1/control-plane/runs/:id/actions".to_string(),
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
    match &endpoint {
        HttpEndpoint::ControlPlaneRun { id } => {
            return control_plane_run_response(endpoint.clone(), id);
        }
        HttpEndpoint::ControlPlaneRunEvents { id, cursor } => {
            return control_plane_events_response(endpoint.clone(), id, cursor.as_ref());
        }
        HttpEndpoint::ControlPlaneCapabilities => {
            return control_plane_capabilities_response();
        }
        HttpEndpoint::ControlPlaneRunActions { id } => {
            return control_plane_action_response(endpoint.clone(), id, request.body.as_ref());
        }
        _ => {}
    }
    // Boundary: one observation-backed HTTP request is one unit of work, so
    // the store is opened exactly once. Control-plane reads return above and
    // never open this unrelated store.
    let store = ObservationStore::open_initialized()?;
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
            "rigs": crate::rig_provider::rig_list_json()?,
        }),
        HttpEndpoint::Rig { id } => json!({
            "command": "api.rigs.show",
            "rig": crate::rig_provider::rig_show_json(id)?,
        }),
        HttpEndpoint::RigCheck { id } => {
            json!({
                "command": "api.rigs.check",
                "report": crate::rig_provider::rig_check_json(id)?,
            })
        }
        HttpEndpoint::Stacks => json!({
            "command": "api.stacks.list",
            "stacks": crate::stack_provider::stack_list_json()?,
        }),
        HttpEndpoint::Stack { id } => json!({
            "command": "api.stacks.show",
            "stack": crate::stack_provider::stack_show_json(id)?,
        }),
        HttpEndpoint::StackStatus { id } => {
            json!({
                "command": "api.stacks.status",
                "report": crate::stack_provider::stack_status_json(id)?,
            })
        }
        HttpEndpoint::Runs => json!({
            "command": "api.runs.list",
            "runs": list_runs(&store, &request.path, None, job_store)?,
            "active_runner_jobs": active_runner_jobs_for_path(&request.path, job_store),
        }),
        HttpEndpoint::Run { id } => json!({
            "command": "api.runs.show",
            "run": show_run(&store, id, job_store)?,
        }),
        HttpEndpoint::RunArtifacts { id } => {
            require_run(&store, id)?;
            // This route predates pagination. Keep an absent pagination request
            // exhaustive for deployed old clients; new CLI clients opt in by
            // sending `limit` and `offset` explicitly.
            if query_value(&request.path, "limit").is_none()
                && query_value(&request.path, "offset").is_none()
            {
                json!({
                    "command": "api.runs.artifacts",
                    "run_id": id,
                    "artifacts": store.list_artifacts(id)?,
                })
            } else {
                let filter = ArtifactListFilter {
                    token: query_value(&request.path, "token"),
                    kind: query_value(&request.path, "kind"),
                    mime: query_value(&request.path, "mime"),
                    original_path: query_value(&request.path, "original_path"),
                    path_suffix: query_value(&request.path, "path_suffix"),
                    fixture: query_value(&request.path, "fixture"),
                    surface: query_value(&request.path, "surface"),
                    scenario: query_value(&request.path, "scenario"),
                    name_glob: query_value(&request.path, "name_glob"),
                    limit: query_value(&request.path, "limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(50),
                    offset: query_value(&request.path, "offset")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                };
                let page = store.list_artifacts_page(id, &filter)?;
                json!({
                    "command": "api.runs.artifacts",
                    "run_id": id,
                    "artifacts": page.artifacts,
                    "page": { "total": page.total, "limit": page.limit, "offset": page.offset,
                        "next_offset": (page.offset + page.artifacts.len() < page.total).then_some(page.offset + page.artifacts.len()) },
                })
            }
        }
        HttpEndpoint::RunArtifactContent { id, artifact_id } => {
            artifact_content(&store, id, artifact_id)?
        }
        HttpEndpoint::RunFindings { id } => {
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
            "runs": list_runs(&store, &request.path, Some("audit"), job_store)?,
        }),
        HttpEndpoint::BenchRuns => json!({
            "command": "api.bench.runs",
            "runs": list_runs(&store, &request.path, Some("bench"), job_store)?,
        }),
        // Deliberately does NOT federate runner-resident records. The daemon
        // serves this on a single-threaded accept loop, so a bounded runner
        // probe -- 15s per connected runner -- would stall `/health`, `daemon
        // status`, and the reverse-broker routes remote runners depend on.
        // The CLI federates by default because it can afford to wait; the
        // daemon cannot. `homeboy activity` remains the federated surface.
        HttpEndpoint::Activity => json!({
            "command": "api.activity.list",
            "activity": activity::activity_report_with(
                activity_scope_for_path(&request.path),
                activity_limit_for_path(&request.path),
                activity::ActivityOptions {
                    federate_runners: false,
                    ..activity::ActivityOptions::default()
                },
            )?,
        }),
        // Same reasoning as `Activity` above: the fallback path in
        // `resolve_activity_item` is *guaranteed* to reach federation for a
        // runner-resident id, so leaving it on would let one lookup stall the
        // whole daemon.
        HttpEndpoint::ActivityItem { id } => json!({
            "command": "api.activity.show",
            "activity": activity::show_activity_with(
                id,
                activity::ActivityOptions {
                    federate_runners: false,
                    ..activity::ActivityOptions::default()
                },
            )?,
        }),
        HttpEndpoint::ControlPlaneRun { .. }
        | HttpEndpoint::ControlPlaneRunEvents { .. }
        | HttpEndpoint::ControlPlaneRunActions { .. }
        | HttpEndpoint::ControlPlaneCapabilities => {
            unreachable!("returned before store open")
        }
        HttpEndpoint::Jobs => {
            let active_runner_jobs = job_store.active_runner_jobs();
            let stale_runner_jobs = job_store.stale_runner_jobs();
            json!({
                "command": "api.jobs.list",
                "jobs": job_store.list(),
                "active_runner_job_count": active_runner_jobs.len(),
                "active_runner_jobs": active_runner_jobs,
                "stale_runner_job_count": stale_runner_jobs.len(),
                "stale_runner_jobs": stale_runner_jobs,
            })
        }
        HttpEndpoint::Job { id } => json!({
            "command": "api.jobs.show",
            "job": job_store.get(parse_job_id(id)?)?,
        }),
        HttpEndpoint::JobEvents { id } => json!({
            "command": "api.jobs.events",
            "job_id": id,
            "events": job_store.events(parse_job_id(id)?)?,
        }),
        HttpEndpoint::JobCancel { id } => {
            let job_id = parse_job_id(id)?;
            json!({
                "command": "api.jobs.cancel",
                "job": job_store.cancel(job_id, "cancel requested via HTTP API")?,
                "events": job_store.events(job_id)?,
            })
        }
        HttpEndpoint::JobProjectionCancel { id } => {
            let job_id = parse_job_id(id)?;
            let request: RunnerJobProjectionCancelRequest = serde_json::from_value(
                request.body.unwrap_or_else(|| json!({})),
            )
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "body",
                    format!("invalid strict runner projection cancellation request: {error}"),
                    Some(id.clone()),
                    None,
                )
            })?;
            json!({
                "command": "api.jobs.cancel_projection",
                "job": job_store.cancel_local_runner_projection(job_id, &request)?,
                "events": job_store.events(job_id)?,
            })
        }
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

/// Upper bound on an accepted agent-task run id.
///
/// The id arrives as a raw URL path segment and is handed to a durable-store
/// lookup. Every real id is a slug or a UUID-suffixed Cook attempt well under
/// this, so the bound costs nothing and keeps an adversarial multi-kilobyte
/// segment from reaching the record store — the same discipline
/// `daemon_endpoint_identity` applies to its nonce.
const MAX_AGENT_TASK_RUN_ID_LEN: usize = 256;

/// Versioned control-plane capability, run, and event reads.
///
/// Route handlers only parse HTTP, call the registered orchestration provider,
/// and serialize the contract.
///
/// # This is a pure read, deliberately
///
/// CLI and HTTP status reads share the same non-reconciling contract. Live
/// runner probes and durable rewrites belong to explicit reconciliation, not
/// the serial daemon accept loop.
fn control_plane_capabilities_response() -> Result<HttpApiResponse> {
    control_plane_ok(
        HttpEndpoint::ControlPlaneCapabilities,
        crate::control_plane::capabilities(),
    )
}

fn control_plane_run_response(endpoint: HttpEndpoint, run_id: &str) -> Result<HttpApiResponse> {
    match control_plane_run(run_id) {
        Ok(resource) => control_plane_ok(endpoint, resource),
        Err(error) => control_plane_err(endpoint, error),
    }
}

fn control_plane_events_response(
    endpoint: HttpEndpoint,
    run_id: &str,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> Result<HttpApiResponse> {
    match control_plane_events(run_id, cursor) {
        Ok(events) => control_plane_ok(endpoint, events),
        Err(error) => control_plane_err(endpoint, error),
    }
}

fn control_plane_action_response(
    endpoint: HttpEndpoint,
    run_id: &str,
    body: Option<&Value>,
) -> Result<HttpApiResponse> {
    let result = body
        .cloned()
        .ok_or_else(|| {
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "control-plane action request body is required",
            )
        })
        .and_then(|body| {
            serde_json::from_value::<homeboy_control_plane_contract::ControlPlaneActionRequest>(
                body,
            )
            .map_err(|error| {
                homeboy_control_plane_contract::ControlPlaneError::invalid_argument(format!(
                    "invalid control-plane action request: {error}"
                ))
            })
        })
        .and_then(|request| {
            control_plane_run_id(run_id)
                .and_then(|run_id| crate::control_plane::execute_action(&run_id, &request))
        });
    match result {
        Ok(acknowledgement) => control_plane_ok(endpoint, acknowledgement),
        Err(error) => control_plane_err(endpoint, error),
    }
}

fn control_plane_run(
    run_id: &str,
) -> std::result::Result<
    homeboy_control_plane_contract::ControlPlaneRun,
    homeboy_control_plane_contract::ControlPlaneError,
> {
    let run_id = control_plane_run_id(run_id)?;
    crate::control_plane::run(&run_id)
}

fn control_plane_run_id(
    run_id: &str,
) -> std::result::Result<
    homeboy_control_plane_contract::RunId,
    homeboy_control_plane_contract::ControlPlaneError,
> {
    if run_id.len() > MAX_AGENT_TASK_RUN_ID_LEN {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(format!(
                "agent-task run id exceeds {MAX_AGENT_TASK_RUN_ID_LEN} bytes"
            )),
        );
    }
    homeboy_control_plane_contract::RunId::new(run_id).map_err(|error| {
        homeboy_control_plane_contract::ControlPlaneError::invalid_argument(error.to_string())
    })
}

fn control_plane_events(
    run_id: &str,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> std::result::Result<
    homeboy_control_plane_contract::ControlPlaneEventPage,
    homeboy_control_plane_contract::ControlPlaneError,
> {
    let requested_id = control_plane_run_id(run_id)?;
    crate::control_plane::events(&requested_id, cursor)
}

fn control_plane_ok<T: serde::Serialize>(
    endpoint: HttpEndpoint,
    resource: T,
) -> Result<HttpApiResponse> {
    let body = serde_json::to_value(homeboy_control_plane_contract::ControlPlaneResult::ok(
        resource,
    ))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize control-plane result".to_string()),
        )
    })?;
    Ok(HttpApiResponse {
        status: 200,
        endpoint: endpoint.name().to_string(),
        body,
    })
}

fn control_plane_err(
    endpoint: HttpEndpoint,
    error: homeboy_control_plane_contract::ControlPlaneError,
) -> Result<HttpApiResponse> {
    let status = error.http_status();
    let body = serde_json::to_value(homeboy_control_plane_contract::ControlPlaneResult::<
        homeboy_control_plane_contract::ControlPlaneRun,
    >::err(error))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize control-plane error".to_string()),
        )
    })?;
    Ok(HttpApiResponse {
        status,
        endpoint: endpoint.name().to_string(),
        body,
    })
}

fn activity_scope_for_path(path: &str) -> activity::ActivityScope {
    if query_value(path, "all").is_some_and(|value| value == "1" || value == "true") {
        activity::ActivityScope::All
    } else {
        activity::ActivityScope::ActiveRecent
    }
}

fn activity_limit_for_path(path: &str) -> usize {
    query_value(path, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|limit| limit.clamp(1, 1000))
        .unwrap_or(20)
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

fn artifact_content(store: &ObservationStore, run_id: &str, artifact_id: &str) -> Result<Value> {
    require_run(store, run_id)?;
    crate::artifacts::index_remote_published_artifact_refs_for_run(&store, run_id)?;
    let decoded_artifact_id = crate::execution_contract::decode_uri_component(artifact_id);
    let Some(artifact) = store.get_artifact_for_run_token(run_id, &decoded_artifact_id)? else {
        return artifact_store_content(
            &store.artifact_root()?,
            run_id,
            artifact_id,
            &decoded_artifact_id,
        );
    };
    if artifact.artifact_type != "file" {
        if artifact.artifact_type == "remote_file"
            || crate::execution_contract::is_remote_runner_artifact_path(&artifact.path)
        {
            let download = crate::observation::runs_service::with_runner_evidence(|provider| {
                provider.download_remote_artifact(&artifact.path, None)
            })?;
            let content = std::fs::read(&download.output_path).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!(
                        "read downloaded remote artifact {}",
                        download.output_path.display()
                    )),
                )
            })?;
            let filename = download
                .output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&artifact.id);
            let size_bytes = download
                .size_bytes
                .or_else(|| i64::try_from(content.len()).ok());
            return Ok(artifact_content_response(
                run_id,
                &artifact.id,
                filename,
                download.content_type.or_else(|| artifact.mime.clone()),
                size_bytes,
                download.sha256.or_else(|| artifact.sha256.clone()),
                &content,
            ));
        }
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
    Ok(artifact_content_response(
        run_id,
        &artifact.id,
        filename,
        artifact.mime,
        artifact.size_bytes,
        artifact.sha256,
        &content,
    ))
}

#[allow(clippy::too_many_arguments)]
fn artifact_content_response(
    run_id: &str,
    artifact_id: &str,
    filename: &str,
    mime: Option<String>,
    size_bytes: Option<i64>,
    sha256: Option<String>,
    content: &[u8],
) -> Value {
    json!({
        "command": "api.runs.artifact.content",
        "run_id": run_id,
        "artifact_id": artifact_id,
        "content_available": true,
        "retrieval": inline_content_retrieval(),
        "filename": filename,
        "mime": mime,
        "size_bytes": size_bytes,
        "sha256": sha256,
        "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
    })
}

fn artifact_store_content(
    artifact_root: &std::path::Path,
    run_id: &str,
    artifact_id: &str,
    decoded_artifact_id: &str,
) -> Result<Value> {
    let locator = crate::execution_contract::artifact_store_locator_from_runner_artifact_id(
        decoded_artifact_id,
    )
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!("artifact record not found: {artifact_id}"),
            Some(artifact_id.to_string()),
            None,
        )
    })?;
    let path = safe_artifact_store_path_in_root(artifact_root, &locator)?;
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
    let sha256 = crate::artifact_metadata::sha256_file(&path).ok();
    Ok(json!({
        "command": "api.runs.artifact.content",
        "run_id": run_id,
        "artifact_id": artifact_id,
        "content_available": true,
        "retrieval": inline_content_retrieval(),
        "filename": filename,
        "mime": crate::artifact_metadata::content_type_from_path(&path),
        "size_bytes": size_bytes,
        "sha256": sha256,
        "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
    }))
}

fn inline_content_retrieval() -> Value {
    json!({
        "mode": "inline_base64",
        "content_available": true,
        "content_field": "content_base64",
        "encoding": "base64",
    })
}

fn safe_artifact_store_path_in_root(
    artifact_root: &std::path::Path,
    locator: &str,
) -> Result<std::path::PathBuf> {
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
    Ok(artifact_root.join(locator_path))
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
    store: &ObservationStore,
    path: &str,
    kind_override: Option<&str>,
    job_store: &JobStore,
) -> Result<Vec<RunSummary>> {
    reconcile_stale_running_runs_for_read(store)?;
    let filter = RunListFilter {
        kind: kind_override
            .map(str::to_string)
            .or_else(|| query_value(path, "kind")),
        component_id: query_value(path, "component").or_else(|| query_value(path, "component_id")),
        status: query_value(path, "status"),
        rig_id: query_value(path, "rig").or_else(|| query_value(path, "rig_id")),
        limit: query_value(path, "limit")
            .and_then(|value| value.parse::<i64>().ok())
            .map(|limit| limit.clamp(1, MAX_RUN_PAGE_LIMIT)),
        // A page ceiling with no way past it is a hard wall; an offset makes
        // the rows beyond it reachable (#11177).
        offset: query_value(path, "offset")
            .and_then(|value| value.parse::<i64>().ok())
            .map(|offset| offset.max(0)),
        ..RunListFilter::default()
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
                .filter_map(active_runner_job_run_summary_if_durable),
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
            Some(status) => status == job.status.run_status_label(),
            None => true,
        })
        .collect();
    if let Some(limit) = limit {
        jobs.truncate(limit);
    }
    jobs
}

fn active_runner_job_run_summary_if_durable(job: ActiveRunnerJobSummary) -> Option<RunSummary> {
    let summary = api_jobs::active_runner_job_run_summary_if_durable(job)?;
    Some(RunSummary {
        id: summary.id,
        kind: summary.kind,
        status: summary.status,
        started_at: summary.started_at,
        finished_at: None,
        component_id: None,
        rig_id: None,
        git_sha: None,
        command: Some(summary.command),
        cwd: summary.cwd,
        status_note: Some(summary.status_note),
    })
}

fn show_run(store: &ObservationStore, run_id: &str, job_store: &JobStore) -> Result<RunDetail> {
    reconcile_stale_running_runs_for_read(store)?;
    if let Some(run) = store.get_run(run_id)? {
        if let Some(job) = active_runner_job_for_durable_run(job_store, run_id) {
            if run.status != RunStatus::Running.as_str() || run_claims_other_runner_job(&run, &job)
            {
                return Ok(active_runner_job_run_detail(run_id, &job, Some(&run)));
            }
        }
        return Ok(RunDetail {
            summary: run_summary(run.clone()),
            homeboy_version: run.homeboy_version,
            metadata: run.metadata_json,
            artifacts: store.list_artifacts(run_id)?,
        });
    }
    if let Some(job) = active_runner_job_for_durable_run(job_store, run_id) {
        return Ok(active_runner_job_run_detail(run_id, &job, None));
    }
    let run = require_run(&store, run_id)?;
    Ok(RunDetail {
        summary: run_summary(run.clone()),
        homeboy_version: run.homeboy_version,
        metadata: run.metadata_json,
        artifacts: store.list_artifacts(run_id)?,
    })
}

fn active_runner_job_for_durable_run(
    job_store: &JobStore,
    run_id: &str,
) -> Option<ActiveRunnerJobSummary> {
    job_store
        .active_runner_jobs()
        .into_iter()
        .find(|job| job.durable_run_id.as_deref() == Some(run_id))
}

fn active_runner_job_run_detail(
    run_id: &str,
    job: &ActiveRunnerJobSummary,
    persisted_run: Option<&RunRecord>,
) -> RunDetail {
    let projection_state = match persisted_run {
        None => "missing_durable_run_record",
        Some(run) if run.status != RunStatus::Running.as_str() => "stale_durable_run_record",
        Some(_) => "foreign_durable_run_record",
    };
    let summary = active_runner_job_run_summary_if_durable(job.clone())
        .expect("active runner job is selected by its durable run id");
    RunDetail {
        summary,
        homeboy_version: None,
        metadata: json!({
            "runner_job_projection": {
                "state": projection_state,
                "durable_run_id": run_id,
                "runner_id": job.runner_id,
                "job_id": job.job_id,
                "message": "The daemon job is active and authoritative; durable-run hydration is unavailable for this projection.",
            }
        }),
        artifacts: Vec::new(),
    }
}

fn run_claims_other_runner_job(run: &RunRecord, job: &ActiveRunnerJobSummary) -> bool {
    ["/lab/remote_job/id", "/lab/remote_job_id"]
        .into_iter()
        .filter_map(|pointer| run.metadata_json.pointer(pointer).and_then(Value::as_str))
        .any(|recorded_job_id| recorded_job_id != job.job_id)
}

fn reconcile_stale_running_runs_for_read(store: &ObservationStore) -> Result<()> {
    for run in store.list_runs(RunListFilter {
        status: Some(RunStatus::Running.as_str().to_string()),
        limit: Some(1000),
        ..RunListFilter::default()
    })? {
        let Some(reason) = api_stale_running_reason(&run) else {
            continue;
        };
        let metadata = api_reconcile_metadata(&run, reason);
        store.finish_run(&run.id, RunStatus::Stale, Some(metadata))?;
    }

    Ok(())
}

fn api_stale_running_reason(run: &RunRecord) -> Option<&'static str> {
    if let Some(owner_pid) = run_owner_pid(run) {
        return (!crate::process::pid_is_running(owner_pid)).then_some("owner_process_not_running");
    }

    api_ownerless_running_is_stale(run).then_some("owner_metadata_missing")
}

fn api_ownerless_running_is_stale(run: &RunRecord) -> bool {
    chrono::DateTime::parse_from_rfc3339(&run.started_at)
        .map(|started_at| {
            chrono::Utc::now()
                .signed_duration_since(started_at.with_timezone(&chrono::Utc))
                .num_minutes()
                >= OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES
        })
        .unwrap_or(false)
}

fn api_reconcile_metadata(run: &RunRecord, reason: &str) -> Value {
    let mut metadata = run.metadata_json.clone();
    let marker = json!({
        "status": RunStatus::Stale.as_str(),
        "reason": reason,
        "owner_pid": run_owner_pid(run).map(|pid| Value::from(pid as u64)).unwrap_or(Value::Null),
        "reconciled_at": chrono::Utc::now().to_rfc3339(),
        "source": "http_api_read_reconcile",
    });

    if let Some(object) = metadata.as_object_mut() {
        object.insert("homeboy_reconciled".to_string(), marker);
        return metadata;
    }

    json!({
        "homeboy_reconciled": marker,
        "homeboy_original_metadata": metadata,
    })
}

/// Exact-id run lookup for HTTP handlers.
///
/// Intentionally **not** routed through
/// [`crate::observation::runs_service::require_run`] (#6768). The facade adds
/// durable label aliasing plus a connected-runner probe that mirrors remote run
/// records into the local store — a network round trip and a write. This module
/// is a read-only, transport-free contract whose handlers must stay latency
/// bounded, so it resolves record ids only. Every other run read path (CLI
/// `runs`, `fuzz`, dossier, evidence) uses the facade.
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
    Uuid::parse_str(job_id).map_err(|error| {
        Error::validation_invalid_argument(
            "job_id",
            format!("invalid job id: {job_id}: {error}"),
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
    let status_note = running_status_note(&run).or_else(|| reconciled_stale_status_note(&run));
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

fn reconciled_stale_status_note(run: &RunRecord) -> Option<String> {
    if run.status != RunStatus::Stale.as_str() {
        return None;
    }

    let reason = run
        .metadata_json
        .get("homeboy_reconciled")?
        .get("reason")?
        .as_str()?;

    match reason {
        "owner_process_not_running" => Some(
            "owner process is not running; run was marked stale during read reconciliation"
                .to_string(),
        ),
        "owner_metadata_missing" => Some(
            "running status had no owner metadata; run was marked stale during read reconciliation"
                .to_string(),
        ),
        "runner_backed_run_exceeded_exemption" => Some(
            "a runner-backed run outlived its reconciliation exemption without recording a terminal outcome; run was marked stale"
                .to_string(),
        ),
        _ => None,
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
#[path = "../../../tests/core/http_api_test.rs"]
mod http_api_test;
