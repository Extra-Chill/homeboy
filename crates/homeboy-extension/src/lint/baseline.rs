//! Lint baseline — delegates to the generic `engine::baseline` primitive.
//!
//! Tracks lint findings emitted by extension sidecar JSON so CI only fails on
//! NEW findings (`id` fingerprints).

use std::path::Path;

use serde::{Deserialize, Serialize};

use homeboy_core::finding::{FindingSource, HomeboyFinding};
use homeboy_core::structured_sidecar;
use homeboy_engine_primitives::baseline::{self as generic, BaselineConfig, Fingerprintable};

const BASELINE_KEY: &str = "lint";

#[cfg(test)]
#[path = "../../../../tests/core/lint_baseline_test.rs"]
mod lint_baseline_test;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintBaselineMetadata {
    pub findings_count: usize,
}

/// The exact lint population a baseline is allowed to describe.
///
/// A changed-file run must never compare itself with an unscoped run: the
/// latter contains findings the former deliberately did not ask producers to
/// inspect. The digest is used as the persisted key while this full value is
/// returned as provenance for humans and automation.
#[derive(Debug, Clone, Serialize)]
pub struct LintBaselineProvenance {
    pub baseline_key: String,
    pub compared: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    pub files: Vec<String>,
    pub tools: Vec<String>,
    pub scope: String,
    pub category: Option<String>,
    pub errors_only: bool,
    pub sniffs: Option<String>,
    pub exclude_sniffs: Option<String>,
}

impl LintBaselineProvenance {
    pub fn new(
        files: Vec<String>,
        tools: Vec<String>,
        scope: impl Into<String>,
        category: Option<String>,
        errors_only: bool,
        sniffs: Option<String>,
        exclude_sniffs: Option<String>,
    ) -> Self {
        let mut files = files;
        files.sort();
        files.dedup();
        let mut tools = tools;
        tools.sort();
        tools.dedup();
        let scope = scope.into();
        let identity = serde_json::json!({
            "files": files,
            "tools": tools,
            "scope": scope,
            "category": category,
            "errors_only": errors_only,
            "sniffs": sniffs,
            "exclude_sniffs": exclude_sniffs,
        });
        let digest = homeboy_engine_primitives::content_hash::sha256_hex(
            serde_json::to_string(&identity)
                .expect("lint baseline provenance is serializable")
                .as_bytes(),
        );
        Self {
            baseline_key: format!("{BASELINE_KEY}:{digest}"),
            compared: false,
            base_ref: None,
            files,
            tools,
            scope,
            category,
            errors_only,
            sniffs,
            exclude_sniffs,
        }
    }

    fn permits_legacy_full_baseline(&self) -> bool {
        self.scope == "full"
            && self.files.is_empty()
            && self.category.is_none()
            && !self.errors_only
            && self.sniffs.is_none()
            && self.exclude_sniffs.is_none()
    }
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

fn config(source_path: &Path, provenance: Option<&LintBaselineProvenance>) -> BaselineConfig {
    BaselineConfig::new(
        source_path,
        provenance
            .map(|provenance| provenance.baseline_key.as_str())
            .unwrap_or(BASELINE_KEY),
    )
}

pub fn parse_findings_file(path: &Path) -> homeboy_core::error::Result<Vec<HomeboyFinding>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        homeboy_core::Error::internal_io(
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

    let mut payload: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        homeboy_core::Error::internal_io(
            format!("Malformed lint findings JSON in {}: {}", path.display(), e),
            Some("lint.baseline.parse".to_string()),
        )
    })?;

    structured_sidecar::validate_payload("lint.findings", &payload)?;

    let serde_json::Value::Array(ref mut raw_findings) = payload else {
        unreachable!("structured sidecar validation guarantees lint findings are an array");
    };

    for finding in raw_findings {
        normalize_legacy_lint_finding(finding);
    }

    let findings: Vec<HomeboyFinding> = serde_json::from_value(payload).map_err(|e| {
        homeboy_core::Error::internal_io(
            format!("Malformed lint findings JSON in {}: {}", path.display(), e),
            Some("lint.baseline.parse".to_string()),
        )
    })?;

    Ok(findings
        .into_iter()
        .map(|finding| normalize_sidecar_finding(finding, path))
        .collect())
}

pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    findings: &[HomeboyFinding],
) -> homeboy_core::error::Result<std::path::PathBuf> {
    save_baseline_for_scope(source_path, component_id, findings, None)
}

pub fn save_baseline_for_scope(
    source_path: &Path,
    component_id: &str,
    findings: &[HomeboyFinding],
    provenance: Option<&LintBaselineProvenance>,
) -> homeboy_core::error::Result<std::path::PathBuf> {
    let config = config(source_path, provenance);
    let metadata = LintBaselineMetadata {
        findings_count: findings.len(),
    };
    let items: Vec<LintFingerprint> = findings.iter().map(LintFingerprint).collect();
    generic::save(&config, component_id, &items, metadata)
}

pub fn load_baseline(source_path: &Path) -> Option<LintBaseline> {
    load_baseline_for_scope(source_path, None)
}

pub fn load_baseline_for_scope(
    source_path: &Path,
    provenance: Option<&LintBaselineProvenance>,
) -> Option<LintBaseline> {
    let config = config(source_path, provenance);
    generic::load::<LintBaselineMetadata>(&config).unwrap_or_default()
}

/// Load the scope-specific baseline, falling back to the pre-scope full-tree
/// baseline only when this run measures that same unfiltered population.
pub fn load_baseline_for_scope_or_legacy_full(
    source_path: &Path,
    provenance: &mut LintBaselineProvenance,
) -> Option<LintBaseline> {
    if let Some(baseline) = load_baseline_for_scope(source_path, Some(provenance)) {
        return Some(baseline);
    }

    if provenance.permits_legacy_full_baseline() {
        if let Some(baseline) = load_baseline(source_path) {
            provenance.baseline_key = BASELINE_KEY.to_string();
            return Some(baseline);
        }
    }

    None
}

pub fn compare(findings: &[HomeboyFinding], baseline: &LintBaseline) -> BaselineComparison {
    let items: Vec<LintFingerprint> = findings.iter().map(LintFingerprint).collect();
    generic::compare(&items, baseline)
}

/// Compare candidate findings with findings measured from an immutable source revision.
pub fn compare_against_findings(
    findings: &[HomeboyFinding],
    baseline_findings: &[HomeboyFinding],
) -> BaselineComparison {
    let baseline = LintBaseline {
        created_at: String::new(),
        context_id: "git-base".to_string(),
        item_count: baseline_findings.len(),
        known_fingerprints: baseline_findings
            .iter()
            .map(|finding| LintFingerprint(finding).fingerprint())
            .collect(),
        metadata: LintBaselineMetadata {
            findings_count: baseline_findings.len(),
        },
    };
    compare(findings, &baseline)
}

fn normalize_sidecar_finding(mut finding: HomeboyFinding, path: &Path) -> HomeboyFinding {
    if finding.source.is_none() {
        finding.source = Some(
            FindingSource::new("sidecar")
                .label("lint-findings")
                .path(path.display().to_string()),
        );
    }
    finding
        .metadata
        .entry("source_sidecar".to_string())
        .or_insert_with(|| serde_json::json!("lint-findings"));
    finding
        .metadata
        .entry("source_sidecar_path".to_string())
        .or_insert_with(|| serde_json::json!(path.display().to_string()));
    finding
}

fn normalize_legacy_lint_finding(finding: &mut serde_json::Value) {
    let Some(object) = finding.as_object_mut() else {
        return;
    };
    let source_kind = object
        .get("source")
        .and_then(|source| source.as_str())
        .map(str::to_string);
    if let Some(kind) = &source_kind {
        object.insert("source".to_string(), serde_json::json!({ "kind": kind }));
    }
    if !object.contains_key("tool") {
        let tool = source_kind
            .or_else(|| {
                object
                    .get("code")
                    .and_then(|code| code.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "lint".to_string());
        object.insert("tool".to_string(), serde_json::json!(tool));
    }
    if !object.contains_key("rule") {
        if let Some(code) = object
            .get("code")
            .and_then(|code| code.as_str())
            .map(str::to_string)
        {
            object.insert("rule".to_string(), serde_json::json!(code));
        }
    }
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
    fn scoped_full_baseline_is_preferred_over_legacy_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut provenance = LintBaselineProvenance::new(
            Vec::new(),
            vec!["eslint".to_string()],
            "full",
            None,
            false,
            None,
            None,
        );
        save_baseline(
            dir.path(),
            "legacy",
            &[lint_finding("legacy", "lint", "legacy")],
        )
        .expect("save legacy baseline");
        save_baseline_for_scope(
            dir.path(),
            "scoped",
            &[lint_finding("scoped", "lint", "scoped")],
            Some(&provenance),
        )
        .expect("save scoped baseline");

        let baseline = load_baseline_for_scope_or_legacy_full(dir.path(), &mut provenance)
            .expect("load scoped baseline");

        assert_eq!(baseline.context_id, "scoped");
        assert_ne!(provenance.baseline_key, BASELINE_KEY);
    }

    #[test]
    fn equivalent_full_scope_falls_back_to_legacy_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut provenance = LintBaselineProvenance::new(
            Vec::new(),
            vec!["eslint".to_string()],
            "full",
            None,
            false,
            None,
            None,
        );
        save_baseline(
            dir.path(),
            "legacy",
            &[lint_finding("legacy", "lint", "legacy")],
        )
        .expect("save legacy baseline");

        let baseline = load_baseline_for_scope_or_legacy_full(dir.path(), &mut provenance)
            .expect("load legacy baseline");

        assert_eq!(baseline.context_id, "legacy");
        assert_eq!(provenance.baseline_key, BASELINE_KEY);
    }

    #[test]
    fn scoped_or_filtered_runs_do_not_fall_back_to_legacy_baseline() {
        let dir = tempfile::tempdir().expect("temp dir");
        save_baseline(
            dir.path(),
            "legacy",
            &[lint_finding("legacy", "lint", "legacy")],
        )
        .expect("save legacy baseline");
        let cases = [
            (
                vec!["changed.rs".to_string()],
                "changed",
                None,
                false,
                None,
                None,
            ),
            (
                vec!["src/lib.rs".to_string()],
                "file",
                None,
                false,
                None,
                None,
            ),
            (
                vec!["src/**/*.rs".to_string()],
                "glob",
                None,
                false,
                None,
                None,
            ),
            (
                Vec::new(),
                "full",
                Some("style".to_string()),
                false,
                None,
                None,
            ),
            (Vec::new(), "full", None, true, None, None),
            (
                Vec::new(),
                "full",
                None,
                false,
                Some("Rule.One".to_string()),
                None,
            ),
            (
                Vec::new(),
                "full",
                None,
                false,
                None,
                Some("Rule.Two".to_string()),
            ),
        ];

        for (files, scope, category, errors_only, sniffs, exclude_sniffs) in cases {
            let mut provenance = LintBaselineProvenance::new(
                files,
                vec!["eslint".to_string()],
                scope,
                category,
                errors_only,
                sniffs,
                exclude_sniffs,
            );
            let scoped_key = provenance.baseline_key.clone();

            assert!(load_baseline_for_scope_or_legacy_full(dir.path(), &mut provenance).is_none());
            assert_eq!(provenance.baseline_key, scoped_key);
        }
    }

    #[test]
    fn test_parse_findings_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("lint-findings.json");
        std::fs::write(
            &path,
            r#"[{"tool":"lint","message":"message","category":"security","fingerprint":"id-1","file":"src/lib.rs"}]"#,
        )
        .expect("findings file written");

        let findings = parse_findings_file(&path).expect("findings parsed");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(findings[0].fingerprint.as_deref(), Some("id-1"));
    }
}
