use serde_json::Value;

use homeboy::core::ci_profile::CiRunOutput;
use homeboy::core::code_audit::AuditCommandOutput;
use homeboy::core::extension::lint::LintCommandOutput;
use homeboy::core::extension::test::TestCommandOutput;
use homeboy::core::finding::HomeboyFinding;

pub(super) trait ReviewArtifactFindings {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        Vec::new()
    }
}

impl ReviewArtifactFindings for AuditCommandOutput {}

impl ReviewArtifactFindings for LintCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        self.findings.clone().unwrap_or_default()
    }
}

impl ReviewArtifactFindings for TestCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        self.findings.clone().unwrap_or_default()
    }
}

impl ReviewArtifactFindings for CiRunOutput {}

impl ReviewArtifactFindings for Value {}
