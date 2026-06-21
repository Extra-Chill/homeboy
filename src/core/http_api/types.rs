//! Request/response and endpoint value types for the local HTTP API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub(super) fn name(&self) -> &'static str {
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
