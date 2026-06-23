use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerDoctorStatus {
    Ok,
    #[serde(rename = "warn")]
    Warning,
    Error,
}

#[derive(Debug, Serialize)]
pub struct RunnerDoctorOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub runner: RunnerTargetSummary,
    pub status: RunnerDoctorStatus,
    pub capabilities: RunnerCapabilities,
    pub resources: RunnerResources,
    pub checks: Vec<RunnerCheck>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repairs: Vec<RunnerRepair>,
}

#[derive(Debug, Serialize)]
pub struct RunnerRepair {
    pub id: String,
    pub status: RunnerDoctorStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerTargetSummary {
    #[serde(rename = "type")]
    pub target_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<RunnerRegistrySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<RunnerServerSummary>,
}

#[derive(Debug, Serialize)]
pub struct RunnerRegistrySummary {
    pub id: String,
    pub kind: RunnerKind,
}

#[derive(Debug, Serialize)]
pub struct RunnerServerSummary {
    pub id: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub is_localhost: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RunnerCapabilities {
    pub local_execution: bool,
    pub ssh_execution: bool,
    pub git: bool,
    pub github_cli: bool,
    pub node: bool,
    pub npm: bool,
    pub pnpm: bool,
    pub php: bool,
    pub composer: bool,
    pub docker: bool,
    pub playwright: bool,
    pub browser_ready: bool,
    pub xvfb_ready: bool,
    pub headed_browser_ready: bool,
    pub workspace_writable: bool,
    pub artifact_store_available: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RunnerResources {
    pub homeboy: HomeboyProbe,
    pub system: SystemProbe,
    pub cpu: CpuProbe,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskProbe>,
    pub workspace_root: String,
    pub artifact_root: String,
    pub tools: BTreeMap<String, ToolProbe>,
}

#[derive(Debug, Default, Serialize)]
pub struct HomeboyProbe {
    pub version: String,
    pub path: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct SystemProbe {
    pub os: String,
    pub arch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct CpuProbe {
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct MemoryProbe {
    pub total_mb: u64,
    pub available_mb: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DiskProbe {
    pub path: String,
    pub total_mb: u64,
    pub available_mb: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolProbe {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerCheck {
    pub id: String,
    pub status: RunnerDoctorStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}
