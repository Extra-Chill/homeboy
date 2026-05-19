//! Lint baseline — delegates to the generic `engine::baseline` primitive.
//!
//! Tracks lint findings emitted by extension sidecar JSON so CI only fails on
//! NEW findings (`id` fingerprints).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::core::engine::baseline::{self as generic, BaselineConfig, Fingerprintable};

const BASELINE_KEY: &str = "lint";

#[cfg(test)]
#[path = "../../../../tests/core/lint_baseline_test.rs"]
mod lint_baseline_test;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LintFinding {
    pub id: String,
    pub message: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
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

pub fn parse_findings_file(path: &Path) -> crate::core::error::Result<Vec<LintFinding>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        crate::core::Error::internal_io(
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
        crate::core::Error::internal_io(
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
) -> crate::core::error::Result<std::path::PathBuf> {
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
            ..LintFinding::default()
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
            ..LintFinding::default()
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
            ..LintFinding::default()
        };
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.context_label(), "lint:security");
    }

    #[test]
    fn test_save_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let finding = LintFinding {
            id: "id-1".to_string(),
            message: "message".to_string(),
            category: "security".to_string(),
            ..LintFinding::default()
        };

        let saved = save_baseline(dir.path(), "homeboy", &[finding]).expect("baseline saved");

        assert!(saved.exists());
    }

    #[test]
    fn test_load_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let finding = LintFinding {
            id: "id-1".to_string(),
            message: "message".to_string(),
            category: "security".to_string(),
            ..LintFinding::default()
        };
        save_baseline(dir.path(), "homeboy", &[finding]).expect("baseline saved");

        let loaded = load_baseline(dir.path()).expect("baseline loaded");

        assert_eq!(loaded.context_id, "homeboy");
        assert_eq!(loaded.item_count, 1);
    }

    #[test]
    fn test_compare() {
        let baseline = generic::Baseline {
            context_id: "homeboy".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            item_count: 1,
            known_fingerprints: vec!["id-1".to_string()],
            metadata: LintBaselineMetadata { findings_count: 1 },
        };
        let findings = vec![
            LintFinding {
                id: "id-1".to_string(),
                message: "message".to_string(),
                category: "security".to_string(),
                ..LintFinding::default()
            },
            LintFinding {
                id: "id-2".to_string(),
                message: "message 2".to_string(),
                category: "i18n".to_string(),
                ..LintFinding::default()
            },
        ];

        let comparison = compare(&findings, &baseline);

        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.new_items[0].fingerprint, "id-2");
    }

    #[test]
    fn test_parse_findings_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("lint-findings.json");
        std::fs::write(
            &path,
            r#"[{"id":"id-1","message":"message","category":"security","file":"src/lib.rs"}]"#,
        )
        .expect("findings file written");

        let findings = parse_findings_file(&path).expect("findings parsed");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file.as_deref(), Some("src/lib.rs"));
    }
}
