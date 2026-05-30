//! Lint baseline — delegates to the generic `engine::baseline` primitive.
//!
//! Tracks lint findings emitted by extension sidecar JSON so CI only fails on
//! NEW findings (`id` fingerprints).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::core::engine::baseline::{self as generic, BaselineConfig, Fingerprintable};
use crate::core::finding::{FindingProducer, FindingSource, HomeboyFinding};

const BASELINE_KEY: &str = "lint";

#[cfg(test)]
#[path = "../../../../tests/core/lint_baseline_test.rs"]
mod lint_baseline_test;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LintSidecarFinding {
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

struct LintFingerprint<'a>(&'a HomeboyFinding);

impl Fingerprintable for LintFingerprint<'_> {
    fn fingerprint(&self) -> String {
        self.0
            .fingerprint
            .clone()
            .unwrap_or_else(|| self.0.message.clone())
    }

    fn description(&self) -> String {
        self.0.message.clone()
    }

    fn context_label(&self) -> String {
        format!(
            "lint:{}",
            self.0.category.as_deref().unwrap_or(self.0.tool.as_str())
        )
    }
}

pub type LintBaseline = generic::Baseline<LintBaselineMetadata>;
pub type BaselineComparison = generic::Comparison;

pub fn parse_findings_file(path: &Path) -> crate::core::error::Result<Vec<HomeboyFinding>> {
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

    let findings: Vec<LintSidecarFinding> = serde_json::from_str(&content).map_err(|e| {
        crate::core::Error::internal_io(
            format!("Malformed lint findings JSON in {}: {}", path.display(), e),
            Some("lint.baseline.parse".to_string()),
        )
    })?;

    Ok(findings
        .into_iter()
        .map(|finding| homeboy_finding_from_sidecar(finding, path))
        .collect())
}

pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    findings: &[HomeboyFinding],
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

pub fn compare(findings: &[HomeboyFinding], baseline: &LintBaseline) -> BaselineComparison {
    let items: Vec<LintFingerprint> = findings.iter().map(LintFingerprint).collect();
    generic::compare(&items, baseline)
}

fn homeboy_finding_from_sidecar(finding: LintSidecarFinding, path: &Path) -> HomeboyFinding {
    let tool = finding.tool.clone().unwrap_or_else(|| "lint".to_string());
    let mut builder = HomeboyFinding::builder(tool.clone(), finding.message.clone())
        .category(finding.category.clone())
        .fingerprint(finding.id.clone())
        .producer(FindingProducer::new(tool.clone()))
        .source(
            FindingSource::new("sidecar")
                .label("lint-findings")
                .path(path.display().to_string()),
        )
        .metadata("source_sidecar", "lint-findings")
        .metadata("source_sidecar_path", path.display().to_string())
        .raw(&finding);

    if let Some(file) = finding.file.clone() {
        builder = builder.file(file);
    }
    if let Some(severity) = finding.severity.clone() {
        builder = builder.severity(severity);
    }
    if let Some(rule) = finding.extra.get("rule").and_then(|value| value.as_str()) {
        builder = builder.rule(rule.to_string());
    } else {
        builder = builder.rule(finding.category.clone());
    }
    if let Some(line) = finding.extra.get("line").and_then(|value| value.as_i64()) {
        builder = builder.line(line);
    }
    if let Some(column) = finding.extra.get("column").and_then(|value| value.as_i64()) {
        builder = builder.column(column);
    }
    if let Some(fixable) = finding
        .extra
        .get("fixable")
        .and_then(|value| value.as_bool())
    {
        builder = builder.fixable(fixable);
    }
    for (key, value) in finding.extra {
        builder = builder.metadata(key, value);
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint_finding(id: &str, category: &str, message: &str) -> HomeboyFinding {
        HomeboyFinding::builder("lint", message)
            .category(category)
            .rule(category)
            .fingerprint(id)
            .build()
    }

    #[test]
    fn test_fingerprint() {
        let finding = lint_finding("id-1", "security", "message");
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.fingerprint(), "id-1");
    }

    #[test]
    fn test_description() {
        let finding = lint_finding("id-1", "security", "message");
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.description(), "message");
    }

    #[test]
    fn test_context_label() {
        let finding = lint_finding("id-1", "security", "message");
        let fp = LintFingerprint(&finding);
        assert_eq!(fp.context_label(), "lint:security");
    }

    #[test]
    fn test_save_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let finding = lint_finding("id-1", "security", "message");

        let saved = save_baseline(dir.path(), "homeboy", &[finding]).expect("baseline saved");

        assert!(saved.exists());
    }

    #[test]
    fn test_load_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let finding = lint_finding("id-1", "security", "message");
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
            lint_finding("id-1", "security", "message"),
            lint_finding("id-2", "i18n", "message 2"),
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
        assert_eq!(findings[0].location.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(findings[0].fingerprint.as_deref(), Some("id-1"));
    }
}
