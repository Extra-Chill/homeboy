//! JSON output envelopes for rig commands.
//!
//! Split from the command handler to keep item counts manageable and so
//! consumers can import a single `RigCommandOutput` enum.

use serde::Serialize;

use homeboy::rig::{self, RigSpec};

/// Tagged union of every rig command's output. `untagged` so each variant
/// serializes to its own shape — consumers discriminate on the `command`
/// field inside the shape.
#[derive(Serialize)]
#[serde(untagged)]
pub enum RigCommandOutput {
    List(RigListOutput),
    Show(RigShowOutput),
    Up(RigUpOutput),
    Check(RigCheckOutput),
    Down(RigDownOutput),
    Sync(RigSyncOutput),
    Status(RigStatusOutput),
    Install(RigInstallOutput),
    Update(RigUpdateOutput),
    Sources(RigSourcesOutput),
    App(RigAppOutput),
}

#[derive(Serialize)]
pub struct RigListOutput {
    pub command: &'static str,
    pub rigs: Vec<RigSummary>,
}

#[derive(Serialize)]
pub struct RigSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_id: Option<String>,
    pub description: String,
    pub component_count: usize,
    pub service_count: usize,
    pub pipelines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<RigSourceSummary>,
}

#[derive(Serialize)]
pub struct RigSourceSummary {
    pub source: String,
    pub package_path: String,
    pub rig_path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Serialize)]
pub struct RigShowOutput {
    pub command: &'static str,
    pub rig: RigSpec,
}

#[derive(Serialize)]
pub struct RigUpOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::UpReport,
}

#[derive(Serialize)]
pub struct RigCheckOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::CheckReport,
}

#[derive(Serialize)]
pub struct RigDownOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::DownReport,
}

#[derive(Serialize)]
pub struct RigSyncOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::RigStackSyncReport,
}

#[derive(Serialize)]
pub struct RigStatusOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::RigStatusReport,
}

#[derive(Serialize)]
pub struct RigInstallOutput {
    pub command: &'static str,
    pub source: String,
    pub package_path: String,
    pub linked: bool,
    pub installed: Vec<RigInstalledSummary>,
}

#[derive(Serialize)]
pub struct RigInstalledSummary {
    pub id: String,
    pub description: String,
    pub path: String,
    pub spec_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Serialize)]
pub struct RigUpdateOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::RigSourceUpdateResult,
}

#[derive(Serialize)]
pub struct RigSourcesOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: RigSourcesReport,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum RigSourcesReport {
    List(rig::RigSourceListResult),
    Remove(rig::RigSourceRemoveResult),
}

#[derive(Serialize)]
pub struct RigAppOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::AppLauncherReport,
}
