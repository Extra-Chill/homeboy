use serde_json::Value;

use homeboy_core::ci_profile::CiRunOutput;
use homeboy_extension_contract::{LintCommandOutput, TestCommandOutput};
use homeboy_finding::HomeboyFinding;

pub trait ReviewArtifactFindings {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        Vec::new()
    }

    fn review_artifacts(&self) -> Vec<Value> {
        Vec::new()
    }
}

impl ReviewArtifactFindings for LintCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        let mut findings = self.findings.clone().unwrap_or_default();
        if let Some(formatting) = &self.formatting_findings {
            findings.extend(formatting.files.iter().map(|file| {
                HomeboyFinding::builder(
                    "format",
                    format!(
                        "Formatting required; run `{}`",
                        formatting.suggested_command
                    ),
                )
                .rule("format-check")
                .category("format")
                .severity("error")
                .file(file.clone())
                .fixable(true)
                .metadata("suggested_command", formatting.suggested_command.clone())
                .build()
            }));
        }
        findings
    }
}

impl ReviewArtifactFindings for TestCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        self.findings.clone().unwrap_or_default()
    }

    fn review_artifacts(&self) -> Vec<Value> {
        self.extension_phase_timings
            .iter()
            .flat_map(|timing| timing.artifacts.iter().cloned())
            .collect()
    }
}

impl ReviewArtifactFindings for CiRunOutput {}

impl ReviewArtifactFindings for Value {}
