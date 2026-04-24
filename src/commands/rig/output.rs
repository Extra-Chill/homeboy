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
    Status(RigStatusOutput),
}

#[derive(Serialize)]
pub struct RigListOutput {
    pub command: &'static str,
    pub rigs: Vec<RigSummary>,
}

#[derive(Serialize)]
pub struct RigSummary {
    pub id: String,
    pub description: String,
    pub component_count: usize,
    pub service_count: usize,
    pub pipelines: Vec<String>,
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
pub struct RigStatusOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::RigStatusReport,
}
