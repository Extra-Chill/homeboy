//! Read-only local HTTP API contract.
//!
//! This module is intentionally transport-free: the daemon can hand it a
//! method/path pair and serialize the returned JSON without duplicating Homeboy
//! command behavior. Long-running analysis endpoints are routed here, but they
//! wait for the job model before execution.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::{component, git, rig, stack};

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
    JobReadyRun { kind: JobReadyRunKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobReadyRunKind {
    Audit,
    Lint,
    Test,
    Bench,
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
            Self::JobReadyRun { .. } => "jobs.required",
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
            ]),
        )),
    }
}

/// Execute a routed read-only API request through existing Homeboy core code.
pub fn handle(request: HttpApiRequest) -> Result<HttpApiResponse> {
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
        HttpEndpoint::JobReadyRun { kind } => {
            return Err(Error::validation_invalid_argument(
                "endpoint",
                format!(
                    "POST /{} requires the HTTP API job model from Extra-Chill/homeboy#1764 before it can run safely",
                    job_ready_slug(*kind)
                ),
                Some(job_ready_slug(*kind).to_string()),
                Some(vec![
                    "Implement the job/event model from Extra-Chill/homeboy#1764 first"
                        .to_string(),
                    "Then wire this endpoint to enqueue the long-running analysis job".to_string(),
                ]),
            ));
        }
    };

    Ok(HttpApiResponse {
        status: 200,
        endpoint: endpoint.name().to_string(),
        body,
    })
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
    }
}

#[cfg(test)]
#[path = "../../tests/core/http_api_test.rs"]
mod http_api_test;
