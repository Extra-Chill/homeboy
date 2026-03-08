//! Lint baseline — delegates to the generic `utils::baseline` primitive.
//!
//! Tracks lint findings emitted by extension sidecar JSON so CI only fails on
//! NEW findings (`id` fingerprints).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::baseline::{self as generic, BaselineConfig, Fingerprintable};

const BASELINE_KEY: &str = "lint";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintFinding {
    pub id: String,
    pub message: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintBaselineMetadata {
    pub findings_count: usize,
}

struct LintFingerprint<'a>(&'a LintFinding);

impl Fingerprintable for LintFingerprint<'_> {
    fn fingerprint(&self) -> String {
        self.0.id.clone()
    }

    fn description(&self) -> String {
        self.0.message.to_string()
    }

    fn context_label(&self) -> String {
        format!("lint:{}", self.0.category)
    }
}

pub type LintBaseline = generic::Baseline<LintBaselineMetadata>;
pub type BaselineComparison = generic::Comparison;

pub fn parse_findings_file(path: &Path) -> crate::error::Result<Vec<LintFinding>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        crate::Error::internal_io(
            format!(
                "Failed to read lint findings file {}: {}",
                path.display(),
                e
            ),
            Some("lint.baseline.parse".to_string()),
        )
    })?;

    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let findings: Vec<LintFinding> = serde_json::from_str(&content).map_err(|e| {
        crate::Error::internal_io(
            format!("Malformed lint findings JSON in {}: {}", path.display(), e),
            Some("lint.baseline.parse".to_string()),
        )
    })?;

    Ok(findings)
}

pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    findings: &[LintFinding],
) -> crate::error::Result<std::path::PathBuf> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    let metadata = LintBaselineMetadata {
        findings_count: findings.len(),
    };
    let items: Vec<LintFingerprint> = findings.iter().map(LintFingerprint).collect();
    generic::save(&config, component_id, &items, metadata)
}

pub fn load_baseline(source_path: &Path) -> Option<LintBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<LintBaselineMetadata>(&config).unwrap_or_default()
}

pub fn compare(findings: &[LintFinding], baseline: &LintBaseline) -> BaselineComparison {
    let items: Vec<LintFingerprint> = findings.iter().map(LintFingerprint).collect();
    generic::compare(&items, baseline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint() {
        let finding = LintFinding {
            id: "id-1".to_string(),
            message: "message".to_string(),
            category: "security".to_string(),
        };
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.fingerprint(), "id-1");
    }

    #[test]
    fn test_description() {
        let finding = LintFinding {
            id: "id-1".to_string(),
            message: "message".to_string(),
            category: "security".to_string(),
        };
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.description(), "message");
    }

    #[test]
    fn test_context_label() {
        let finding = LintFinding {
            id: "id-1".to_string(),
            message: "message".to_string(),
            category: "security".to_string(),
        };
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.context_label(), "lint:security");
    }
}
