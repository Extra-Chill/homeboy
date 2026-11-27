//! JSON output envelopes for rig commands.
//!
//! Split from the command handler to keep item counts manageable and so
//! consumers can import a single `RigCommandOutput` enum.

use serde::Serialize;

use crate::commands::bench::{BenchOutput, RigRunBenchPlan};
use crate::commands::CommandReport;
use homeboy::core::EntityRows;
use homeboy::rig::{self, RigResourcesSpec, RigSpec};

/// Tagged union of every rig command's output.
#[derive(Serialize)]
#[serde(tag = "variant", content = "payload", rename_all = "snake_case")]
pub enum RigCommandOutput {
    List(RigListOutput),
    Show(Box<RigShowOutput>),
    Materialize(CommandReport<rig::MaterializedRigResource>),
    Up(RigUpOutput),
    UpPlan(RigUpPlanOutput),
    Check(RigCheckOutput),
    Down(RigDownOutput),
    Repair(RigRepairOutput),
    Status(RigStatusOutput),
    Install(RigInstallOutput),
    Update(RigUpdateOutput),
    Sources(RigSourcesOutput),
    App(RigAppOutput),
    ArtifactRegister(RigArtifactRegisterOutput),
    ReleaseLock(RigReleaseLockOutput),
    Run(Box<RigRunOutput>),
}

#[derive(Serialize)]
pub struct RigArtifactRegisterOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub registration: rig::LocalArtifactRegistration,
}

#[derive(Serialize)]
pub struct RigRunOutput {
    pub command: &'static str,
    pub rig_id: String,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_update: Option<rig::RigSourceUpdateResult>,
    pub bench_invocation: RigRunBenchPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bench: Option<BenchOutput>,
}

pub type RigReleaseLockOutput = CommandReport<rig::ReleaseLeaseOutcome>;

#[derive(Serialize)]
pub struct RigListOutput {
    pub command: &'static str,
    /// Rows under `entities` (#14876).
    #[serde(flatten)]
    pub rigs: EntityRows<RigSummary>,
}

impl RigListOutput {
    pub fn new(rigs: Vec<RigSummary>) -> Self {
        Self {
            command: "rig.list",
            rigs: EntityRows::new(rigs),
        }
    }
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
    #[serde(skip_serializing_if = "RigResourcesSpec::is_unset")]
    pub resources: RigResourcesSpec,
}

pub type RigUpOutput = CommandReport<rig::UpReport>;

#[derive(Serialize)]
pub struct RigUpPlanOutput {
    pub command: &'static str,
    pub rig_id: String,
    pub runner_id: String,
    pub selected_homeboy_binary: String,
    pub portable: bool,
    pub steps: Vec<RigUpPlanStep>,
    pub commands: Vec<String>,
}

#[derive(Serialize)]
pub struct RigUpPlanStep {
    pub label: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    pub runner_exec_command: String,
}

pub type RigCheckOutput = CommandReport<rig::CheckReport>;
pub type RigDownOutput = CommandReport<rig::DownReport>;
pub type RigRepairOutput = CommandReport<rig::RepairReport>;
pub type RigStatusOutput = CommandReport<rig::RigStatusReport>;

#[derive(Serialize)]
pub struct RigInstallOutput {
    pub command: &'static str,
    pub source: String,
    pub package_path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub source_dirty: bool,
    pub source_content_hash: String,
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

pub type RigUpdateOutput = CommandReport<rig::RigSourceUpdateResult>;
pub type RigSourcesOutput = CommandReport<RigSourcesReport>;

#[derive(Serialize)]
#[serde(tag = "variant", content = "payload", rename_all = "snake_case")]
pub enum RigSourcesReport {
    List(rig::RigSourceListResult),
    Remove(Box<rig::RigSourceRemoveResult>),
    Refresh(rig::RigSourceUpdateResult),
}

pub type RigAppOutput = CommandReport<rig::AppLauncherReport>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rig_list_returns_rows_under_entities() {
        let output = RigCommandOutput::List(RigListOutput::new(vec![RigSummary {
            id: "studio-bfb".to_string(),
            declared_id: None,
            description: "Studio".to_string(),
            component_count: 2,
            service_count: 1,
            pipelines: vec!["up".to_string()],
            source: None,
        }]));

        let value = serde_json::to_value(&output).expect("serialize rig list");

        assert_eq!(value["variant"], "list");
        assert_eq!(value["payload"]["command"], "rig.list");
        assert_eq!(value["payload"]["entities"][0]["id"], "studio-bfb");

        let empty = serde_json::to_value(RigCommandOutput::List(RigListOutput::new(Vec::new())))
            .expect("serialize empty rig list");
        assert_eq!(empty["payload"]["entities"], serde_json::json!([]));
    }

    #[test]
    fn rig_show_output_includes_expanded_resources() {
        let output = RigShowOutput {
            command: "rig.show",
            rig: RigSpec {
                id: "studio-bfb".to_string(),
                ..Default::default()
            },
            resources: RigResourcesSpec {
                exclusive: vec!["studio-runtime".to_string()],
                paths: vec!["/Users/user/Developer/studio".to_string()],
                ..Default::default()
            },
        };

        let value = serde_json::to_value(&output).expect("serialize");
        assert_eq!(value["command"], "rig.show");
        assert_eq!(
            value["resources"]["paths"],
            serde_json::json!(["/Users/user/Developer/studio"])
        );
        assert_eq!(
            value["resources"]["exclusive"],
            serde_json::json!(["studio-runtime"])
        );
    }

    #[test]
    fn rig_materialize_output_uses_the_machine_readable_variant() {
        let output = RigCommandOutput::Materialize(CommandReport {
            command: "rig.materialize",
            report: rig::MaterializedRigResource {
                schema: rig::MATERIALIZED_RIG_RESOURCE_SCHEMA.to_string(),
                rig_id: "example".to_string(),
                materialized_rig_json_sha256: "sha256:abc123".to_string(),
                rig: serde_json::json!({ "id": "example" }),
            },
        });

        assert_eq!(
            serde_json::to_value(output).expect("serialize"),
            serde_json::json!({
                "variant": "materialize",
                "payload": {
                    "command": "rig.materialize",
                    "schema": "homeboy/materialized-rig/v1",
                    "rig_id": "example",
                    "materialized_rig_json_sha256": "sha256:abc123",
                    "rig": { "id": "example" }
                }
            })
        );
    }
}
