//! Audit-specific baseline — delegates to the generic `engine::baseline` primitive.
//!
//! Provides the audit domain's [`Fingerprintable`] implementation for findings,
//! plus backward-compatible wrappers (`save_baseline`, `load_baseline`, `compare`)
//! that the audit command uses directly.

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::engine::baseline::{self as generic, BaselineConfig, Fingerprintable};

use super::conventions::AuditFinding as AuditFindingKind;
use super::findings::Finding;
use super::CodeAuditResult;

// ============================================================================
// Baseline key
// ============================================================================

/// Key used under `baselines.audit`.
const BASELINE_KEY: &str = "audit";

// ============================================================================
// Audit-specific metadata
// ============================================================================

/// Domain-specific metadata stored alongside the generic baseline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditBaselineMetadata {
    /// Total outlier files at baseline time.
    pub outliers_count: usize,
    /// Alignment score at baseline time.
    pub alignment_score: Option<f32>,
    /// Set of known outlier file paths (accepted drift).
    pub known_outliers: Vec<String>,
    /// Scoped audit-policy sections for easier baseline review and ratcheting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_sections: Vec<AuditBaselinePolicySection>,
}

/// A scoped slice of audit baseline fingerprints.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AuditBaselinePolicySection {
    /// Stable section key, usually `<audit_policy>/<scope>`.
    pub key: String,
    /// Audit policy/convention label that owns these fingerprints.
    pub audit_policy: String,
    /// Optional policy scope within the audit policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Optional issue URL or reference tracking this accepted debt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    /// Optional rationale for why this section is baselined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Known fingerprints for this policy/scope section.
    pub known_fingerprints: Vec<String>,
}

/// Baseline mode for resolved/stale policy fingerprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyBaselineMode {
    /// Full-policy runs should fail on stale baseline rows so cleanup ratchets.
    Full,
    /// Changed-scope runs ignore stale rows outside the active scope.
    ChangedScope,
}

/// Current source-policy/core-boundary section used by the core-agnostic tests.
pub const SOURCE_POLICY_CORE_BOUNDARY_POLICY: &str = "core_boundary_leak:core-agnostic-source";
pub const SOURCE_POLICY_CORE_BOUNDARY_SCOPE: &str = "core-boundary";

// ============================================================================
// Fingerprintable implementation for audit findings
// ============================================================================

/// Wrapper that implements [`Fingerprintable`] for audit findings.
///
/// Uses `convention::file::kind` as the core identity. The description is
/// excluded for most findings because structural findings embed volatile values
/// (e.g. exact line counts) that change when a file grows by even one line.
/// Core-boundary leaks include the configured policy, term, and line in their
/// description so each configured source-policy finding can ratchet normally.
struct AuditFinding<'a>(&'a Finding);

impl Fingerprintable for AuditFinding<'_> {
    fn fingerprint(&self) -> String {
        let file = if self.0.kind == AuditFindingKind::NonPortableArtifactPath {
            artifact_portability_baseline_file(self.0)
        } else {
            self.0.file.clone()
        };

        if self.0.kind == AuditFindingKind::CoreBoundaryLeak {
            return format!(
                "{}::{}::{}::{:?}",
                self.0.convention, file, self.0.description, self.0.kind
            );
        }

        format!("{}::{}::{:?}", self.0.convention, file, self.0.kind)
    }

    fn description(&self) -> String {
        self.0.description.clone()
    }

    fn context_label(&self) -> String {
        self.0.convention.clone()
    }
}

fn artifact_portability_baseline_file(finding: &Finding) -> String {
    let issue = if finding
        .description
        .contains("without a mirrored artifact record")
    {
        "missing_mirrored_artifact"
    } else if finding.description.starts_with("Artifact ") {
        "artifact_path"
    } else if finding
        .description
        .contains("records local-only artifact path")
    {
        "local_artifact_path"
    } else {
        "non_portable_artifact_path"
    };
    let field = extract_backtick_value_after(&finding.description, "field `")
        .unwrap_or("unknown_field")
        .replace("::", ":");

    format!("artifact_portability/{issue}/{field}")
}

fn extract_backtick_value_after<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    let start = value.find(marker)? + marker.len();
    let rest = &value[start..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

// ============================================================================
// Backward-compatible public types
// ============================================================================

/// A saved baseline snapshot (backward-compatible alias).
///
/// This is the generic baseline parameterized with audit metadata.
pub type AuditBaseline = generic::Baseline<AuditBaselineMetadata>;

/// Result of comparing an audit against a baseline.
pub type BaselineComparison = generic::Comparison;

/// A finding that wasn't in the baseline.
pub type NewFinding = generic::NewItem;

// ============================================================================
// Backward-compatible public API
// ============================================================================

/// Save the current audit result as a baseline.
pub fn save_baseline(result: &CodeAuditResult) -> Result<std::path::PathBuf, String> {
    let source = Path::new(&result.source_path);
    let config = BaselineConfig::new(source, BASELINE_KEY);

    let known_outliers: Vec<String> = result
        .conventions
        .iter()
        .flat_map(|c| c.outliers.iter().map(|o| o.file.clone()))
        .collect();

    let items: Vec<AuditFinding> = result.findings.iter().map(AuditFinding).collect();
    let known_fingerprints = fingerprints_for_items(&items);
    let metadata = AuditBaselineMetadata {
        outliers_count: known_outliers.len(),
        alignment_score: result.summary.alignment_score,
        known_outliers,
        policy_sections: policy_sections_from_fingerprints(&known_fingerprints),
    };

    generic::save(&config, &result.component_id, &items, metadata).map_err(|e| e.message)
}

/// Save a scoped baseline update — merges with existing baseline instead of replacing.
///
/// Only fingerprints for files in `changed_files` are updated:
/// - Removes old fingerprints for files in scope
/// - Adds current fingerprints from the scoped audit result
/// - Preserves all fingerprints outside the scope untouched
///
/// This prevents CI/local environment parity from causing baseline churn
/// on files that weren't part of the current change set.
pub fn save_baseline_scoped(
    result: &CodeAuditResult,
    changed_files: &[String],
) -> Result<std::path::PathBuf, String> {
    let source = Path::new(&result.source_path);
    let config = BaselineConfig::new(source, BASELINE_KEY);

    let known_outliers: Vec<String> = result
        .conventions
        .iter()
        .flat_map(|c| c.outliers.iter().map(|o| o.file.clone()))
        .collect();

    let items: Vec<AuditFinding> = result.findings.iter().map(AuditFinding).collect();
    let merged_fingerprints = scoped_policy_fingerprints(&config, &items, changed_files);
    let metadata = AuditBaselineMetadata {
        outliers_count: known_outliers.len(),
        alignment_score: result.summary.alignment_score,
        known_outliers,
        policy_sections: policy_sections_from_fingerprints(&merged_fingerprints),
    };

    generic::save_scoped(
        &config,
        &result.component_id,
        &items,
        metadata,
        changed_files,
        file_from_audit_fingerprint,
    )
    .map_err(|e| e.message)
}

/// Extract the file path from an audit fingerprint.
///
/// Audit fingerprints have the format `convention::file::kind`.
/// The file path is the middle segment between the first `::` and the last `::`.
pub fn file_from_audit_fingerprint(fingerprint: &str) -> Option<String> {
    let first = fingerprint.find("::")?;
    let rest = &fingerprint[first + 2..];
    let last = rest.rfind("::")?;
    Some(rest[..last].to_string())
}

/// Load a baseline if one exists for the given source path.
pub fn load_baseline(source_path: &Path) -> Option<AuditBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<AuditBaselineMetadata>(&config)
        .ok()
        .flatten()
        .map(normalize_loaded_baseline)
}

/// Compare an audit result against a saved baseline.
pub fn compare(result: &CodeAuditResult, baseline: &AuditBaseline) -> BaselineComparison {
    let items: Vec<AuditFinding> = result.findings.iter().map(AuditFinding).collect();
    generic::compare(&items, baseline)
}

/// Return the audit-baseline identity for one finding.
pub fn finding_baseline_fingerprint(finding: &Finding) -> String {
    AuditFinding(finding).fingerprint()
}

/// Load an audit baseline from a git ref (e.g., `origin/main`).
///
/// Uses `git show` to read the persisted baseline without checkout.
/// Returns `None` if the ref doesn't have a baseline.
pub fn load_baseline_from_ref(source_path: &str, git_ref: &str) -> Option<AuditBaseline> {
    generic::load_from_git_ref::<AuditBaselineMetadata>(source_path, git_ref, BASELINE_KEY)
        .map(normalize_loaded_baseline)
}

/// Return known fingerprints for a scoped policy section, falling back to the legacy flat list.
pub fn policy_baseline_fingerprints<'a>(
    baseline: &'a AuditBaseline,
    audit_policy: &str,
    scope: Option<&str>,
) -> BTreeSet<&'a str> {
    let mut fingerprints = baseline
        .known_fingerprints
        .iter()
        .filter(|fingerprint| fingerprint.starts_with(audit_policy))
        .map(|fingerprint| fingerprint.as_str())
        .collect::<BTreeSet<_>>();

    if let Some(section) =
        baseline.metadata.policy_sections.iter().find(|section| {
            section.audit_policy == audit_policy && section.scope.as_deref() == scope
        })
    {
        fingerprints.extend(
            section
                .known_fingerprints
                .iter()
                .map(|fingerprint| fingerprint.as_str()),
        );
    }

    fingerprints
}

/// Return stale baseline rows for a policy section according to the requested baseline mode.
pub fn stale_policy_baseline_fingerprints(
    baseline: &AuditBaseline,
    current_policy_fingerprints: &BTreeSet<String>,
    audit_policy: &str,
    scope: Option<&str>,
    mode: PolicyBaselineMode,
) -> Vec<String> {
    if mode == PolicyBaselineMode::ChangedScope {
        return Vec::new();
    }

    policy_baseline_fingerprints(baseline, audit_policy, scope)
        .into_iter()
        .filter(|fingerprint| !current_policy_fingerprints.contains(*fingerprint))
        .map(str::to_string)
        .collect()
}

fn fingerprints_for_items(items: &[AuditFinding]) -> Vec<String> {
    let mut known_fingerprints = items
        .iter()
        .map(|item| item.fingerprint())
        .collect::<Vec<_>>();
    known_fingerprints.sort();
    known_fingerprints.dedup();
    known_fingerprints
}

fn scoped_policy_fingerprints(
    config: &BaselineConfig,
    items: &[AuditFinding],
    changed_files: &[String],
) -> Vec<String> {
    let current = fingerprints_for_items(items);
    let Ok(Some(existing)) = generic::load::<AuditBaselineMetadata>(config) else {
        return current;
    };

    let changed = changed_files
        .iter()
        .map(|file| file.as_str())
        .collect::<BTreeSet<_>>();
    let mut merged = existing
        .known_fingerprints
        .into_iter()
        .filter(|fingerprint| {
            file_from_audit_fingerprint(fingerprint)
                .as_deref()
                .is_none_or(|file| !changed.contains(file))
        })
        .collect::<Vec<_>>();
    merged.extend(current);
    merged.sort();
    merged.dedup();
    merged
}

fn normalize_loaded_baseline(mut baseline: AuditBaseline) -> AuditBaseline {
    let section_fingerprints = baseline
        .metadata
        .policy_sections
        .iter()
        .flat_map(|section| section.known_fingerprints.iter().cloned())
        .collect::<Vec<_>>();
    if !section_fingerprints.is_empty() {
        baseline.known_fingerprints.extend(section_fingerprints);
        baseline.known_fingerprints.sort();
        baseline.known_fingerprints.dedup();
        baseline.item_count = baseline.known_fingerprints.len();
    }
    baseline
}

fn policy_sections_from_fingerprints(fingerprints: &[String]) -> Vec<AuditBaselinePolicySection> {
    let core_boundary = fingerprints
        .iter()
        .filter(|fingerprint| fingerprint.starts_with(SOURCE_POLICY_CORE_BOUNDARY_POLICY))
        .cloned()
        .collect::<Vec<_>>();

    if core_boundary.is_empty() {
        return Vec::new();
    }

    vec![AuditBaselinePolicySection {
        key: format!(
            "{}/{}",
            SOURCE_POLICY_CORE_BOUNDARY_POLICY, SOURCE_POLICY_CORE_BOUNDARY_SCOPE
        ),
        audit_policy: SOURCE_POLICY_CORE_BOUNDARY_POLICY.to_string(),
        scope: Some(SOURCE_POLICY_CORE_BOUNDARY_SCOPE.to_string()),
        issue: Some("#3498".to_string()),
        rationale: Some(
            "Scoped source-policy/core-boundary debt baseline for reviewable ratchets.".to_string(),
        ),
        known_fingerprints: core_boundary,
    }]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::AuditFinding;
    use crate::core::code_audit::findings::{Finding, Severity};
    use crate::core::code_audit::{AuditSummary, CodeAuditResult};

    fn make_finding(convention: &str, file: &str, description: &str) -> Finding {
        Finding {
            convention: convention.to_string(),
            severity: Severity::Warning,
            file: file.to_string(),
            description: description.to_string(),
            suggestion: String::new(),
            kind: AuditFinding::MissingMethod,
        }
    }

    fn make_finding_with_kind(
        convention: &str,
        file: &str,
        description: &str,
        kind: AuditFinding,
    ) -> Finding {
        Finding {
            convention: convention.to_string(),
            severity: Severity::Warning,
            file: file.to_string(),
            description: description.to_string(),
            suggestion: String::new(),
            kind,
        }
    }

    fn make_result(findings: Vec<Finding>, test_name: &str) -> CodeAuditResult {
        let dir = std::env::temp_dir().join(format!("audit_baseline_{}", test_name));
        let _ = std::fs::remove_dir_all(&dir); // Clean slate
        let _ = std::fs::create_dir_all(&dir);
        CodeAuditResult {
            component_id: "test".to_string(),
            source_path: dir.to_str().unwrap().to_string(),
            summary: AuditSummary {
                files_scanned: 10,
                conventions_detected: 1,
                outliers_found: findings.len(),
                alignment_score: Some(0.8),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings,
            duplicate_groups: vec![],
        }
    }

    #[test]
    fn save_and_load_baseline() {
        let result = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "save_load",
        );

        let path = save_baseline(&result).unwrap();
        assert!(path.exists());

        let loaded = load_baseline(Path::new(&result.source_path)).unwrap();
        assert_eq!(loaded.context_id, "test");
        assert_eq!(loaded.item_count, 2);
        assert_eq!(loaded.known_fingerprints.len(), 2);

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn compare_no_new_drift() {
        let result = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "no_new_drift",
        );
        let _ = save_baseline(&result).unwrap();
        let baseline = load_baseline(Path::new(&result.source_path)).unwrap();

        let comparison = compare(&result, &baseline);
        assert!(!comparison.drift_increased);
        assert!(comparison.new_items.is_empty());
        assert!(comparison.resolved_fingerprints.is_empty());
        assert_eq!(comparison.delta, 0);

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn compare_detects_new_drift() {
        let result_original = make_result(
            vec![make_finding("Flow", "a.php", "Missing method: execute")],
            "new_drift",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline = load_baseline(Path::new(&result_original.source_path)).unwrap();

        // New finding added — reuse same source_path
        let mut current = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "c.php", "Missing method: register"),
            ],
            "new_drift_current",
        );
        current.source_path = result_original.source_path.clone();

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(
            comparison.new_items[0].fingerprint,
            "Flow::c.php::MissingMethod"
        );
        assert_eq!(comparison.delta, 1);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn compare_detects_resolved_drift() {
        let result_original = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "resolved_drift",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline = load_baseline(Path::new(&result_original.source_path)).unwrap();

        let mut current = make_result(
            vec![make_finding("Flow", "a.php", "Missing method: execute")],
            "resolved_drift_current",
        );
        current.source_path = result_original.source_path.clone();

        let comparison = compare(&current, &baseline);
        assert!(!comparison.drift_increased);
        assert!(comparison.new_items.is_empty());
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
        assert_eq!(comparison.delta, -1);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn compare_new_and_resolved_simultaneously() {
        let result_original = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "new_and_resolved",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline = load_baseline(Path::new(&result_original.source_path)).unwrap();

        // b.php fixed, but c.php introduced
        let mut current = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "c.php", "Missing method: register"),
            ],
            "new_and_resolved_current",
        );
        current.source_path = result_original.source_path.clone();

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
        assert_eq!(comparison.delta, 0);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn auto_ratchet_saves_updated_baseline_after_resolving_findings() {
        // Simulates the auto-ratchet flow:
        // 1. Save baseline with 3 findings
        // 2. "Fix" resolves 1 finding (current has 2)
        // 3. Re-save baseline from current state
        // 4. Verify baseline now has 2 findings
        let result_original = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
                make_finding("Flow", "c.php", "Missing method: register"),
            ],
            "auto_ratchet",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline_before = load_baseline(Path::new(&result_original.source_path)).unwrap();
        assert_eq!(baseline_before.item_count, 3);

        // After autofix: c.php finding was resolved
        let mut current = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "auto_ratchet_current",
        );
        current.source_path = result_original.source_path.clone();

        // Compare detects resolved findings
        let comparison = compare(&current, &baseline_before);
        assert!(!comparison.drift_increased);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);

        // Auto-ratchet: save updated baseline
        let _ = save_baseline(&current).unwrap();
        let baseline_after = load_baseline(Path::new(&current.source_path)).unwrap();
        assert_eq!(baseline_after.item_count, 2);

        // Verify the resolved finding is gone from the new baseline
        let recheck = compare(&current, &baseline_after);
        assert!(!recheck.drift_increased);
        assert!(recheck.resolved_fingerprints.is_empty());
        assert_eq!(recheck.delta, 0);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn auto_ratchet_preserves_baseline_when_no_findings_resolved() {
        let result = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "auto_ratchet_no_change",
        );
        let _ = save_baseline(&result).unwrap();
        let baseline_before = load_baseline(Path::new(&result.source_path)).unwrap();

        // Same findings — nothing resolved
        let comparison = compare(&result, &baseline_before);
        assert!(comparison.resolved_fingerprints.is_empty());
        assert!(!comparison.drift_increased);

        // Baseline should NOT be re-saved (unchanged)
        // The auto-ratchet code checks resolved_fingerprints.is_empty()
        // and skips the save in that case

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn no_baseline_returns_none() {
        let result = load_baseline(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }

    #[test]
    fn audit_metadata_roundtrips() {
        let result = make_result(
            vec![make_finding("Flow", "a.php", "Missing method")],
            "metadata_roundtrip",
        );

        let _ = save_baseline(&result).unwrap();
        let loaded = load_baseline(Path::new(&result.source_path)).unwrap();

        assert_eq!(loaded.metadata.alignment_score, Some(0.8));

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn source_policy_core_boundary_baseline_section_roundtrips() {
        let result = make_result(
            vec![make_finding_with_kind(
                SOURCE_POLICY_CORE_BOUNDARY_POLICY,
                "src/core/example.rs",
                "Core boundary leak (core-agnostic-source) configured ecosystem term `ProductName` appears at line 7 in behavioral context `top-level`",
                AuditFinding::CoreBoundaryLeak,
            )],
            "policy_section_roundtrip",
        );

        let _ = save_baseline(&result).unwrap();
        let loaded = load_baseline(Path::new(&result.source_path)).unwrap();

        let fingerprints = policy_baseline_fingerprints(
            &loaded,
            SOURCE_POLICY_CORE_BOUNDARY_POLICY,
            Some(SOURCE_POLICY_CORE_BOUNDARY_SCOPE),
        );
        assert_eq!(fingerprints.len(), 1);
        assert_eq!(loaded.metadata.policy_sections.len(), 1);
        assert!(loaded.metadata.policy_sections[0]
            .issue
            .as_deref()
            .unwrap_or_default()
            .ends_with("#3498"));

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn policy_baseline_fingerprints_falls_back_to_legacy_flat_list() {
        let fingerprint = format!(
            "{}::src/core/example.rs::Core boundary leak (core-agnostic-source) configured ecosystem term `ProductName` appears at line 7 in behavioral context `top-level`::CoreBoundaryLeak",
            SOURCE_POLICY_CORE_BOUNDARY_POLICY
        );
        let baseline = AuditBaseline {
            created_at: "2026-06-04T00:00:00Z".to_string(),
            context_id: "test".to_string(),
            item_count: 1,
            known_fingerprints: vec![fingerprint.clone()],
            metadata: AuditBaselineMetadata {
                outliers_count: 1,
                alignment_score: None,
                known_outliers: vec![],
                policy_sections: vec![],
            },
        };

        let fingerprints = policy_baseline_fingerprints(
            &baseline,
            SOURCE_POLICY_CORE_BOUNDARY_POLICY,
            Some(SOURCE_POLICY_CORE_BOUNDARY_SCOPE),
        );
        assert!(fingerprints.contains(fingerprint.as_str()));
    }

    #[test]
    fn changed_scope_policy_mode_ignores_stale_section_rows() {
        let fingerprint = format!(
            "{}::src/core/example.rs::Core boundary leak (core-agnostic-source) configured ecosystem term `ProductName` appears at line 7 in behavioral context `top-level`::CoreBoundaryLeak",
            SOURCE_POLICY_CORE_BOUNDARY_POLICY
        );
        let baseline = AuditBaseline {
            created_at: "2026-06-04T00:00:00Z".to_string(),
            context_id: "test".to_string(),
            item_count: 1,
            known_fingerprints: vec![fingerprint.clone()],
            metadata: AuditBaselineMetadata {
                outliers_count: 1,
                alignment_score: None,
                known_outliers: vec![],
                policy_sections: policy_sections_from_fingerprints(&[fingerprint.clone()]),
            },
        };
        let current = BTreeSet::new();

        assert_eq!(
            stale_policy_baseline_fingerprints(
                &baseline,
                &current,
                SOURCE_POLICY_CORE_BOUNDARY_POLICY,
                Some(SOURCE_POLICY_CORE_BOUNDARY_SCOPE),
                PolicyBaselineMode::ChangedScope,
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            stale_policy_baseline_fingerprints(
                &baseline,
                &current,
                SOURCE_POLICY_CORE_BOUNDARY_POLICY,
                Some(SOURCE_POLICY_CORE_BOUNDARY_SCOPE),
                PolicyBaselineMode::Full,
            ),
            vec![fingerprint]
        );
    }

    #[test]
    fn fingerprint_is_stable() {
        let f1 = make_finding("Flow", "a.php", "Missing method: execute");
        let f2 = make_finding("Flow", "a.php", "Missing method: execute");
        assert_eq!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f2).fingerprint()
        );

        // Different file = different fingerprint
        let f3 = make_finding("Flow", "b.php", "Missing method: execute");
        assert_ne!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f3).fingerprint()
        );
    }

    #[test]
    fn fingerprint_ignores_description() {
        let f1 = Finding {
            convention: "structural".to_string(),
            severity: Severity::Warning,
            file: "deploy.rs".to_string(),
            description: "File has 2484 lines (threshold: 1000)".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::GodFile,
        };
        let f2 = Finding {
            convention: "structural".to_string(),
            severity: Severity::Warning,
            file: "deploy.rs".to_string(),
            description: "File has 2645 lines (threshold: 1000)".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::GodFile,
        };
        assert_eq!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f2).fingerprint(),
            "fingerprint should not change when line count changes"
        );
    }

    #[test]
    fn artifact_portability_fingerprint_ignores_observation_run_and_path() {
        let f1 = make_finding_with_kind(
            "artifact_portability",
            "observation:run-a",
            "Run metadata field $.run_dir records local-only artifact path /tmp/run-a; command `tool test`; field `$.run_dir`",
            AuditFinding::NonPortableArtifactPath,
        );
        let f2 = make_finding_with_kind(
            "artifact_portability",
            "observation:run-b",
            "Run metadata field $.run_dir records local-only artifact path /Users/chris/.config/tool/runtime/tmp/run-b; command `tool lint`; field `$.run_dir`",
            AuditFinding::NonPortableArtifactPath,
        );

        assert_eq!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f2).fingerprint()
        );
        assert_eq!(
            AuditFinding(&f1).fingerprint(),
            "artifact_portability::artifact_portability/local_artifact_path/$.run_dir::NonPortableArtifactPath"
        );
    }

    #[test]
    fn artifact_portability_fingerprint_distinguishes_violation_shape() {
        let local_path = make_finding_with_kind(
            "artifact_portability",
            "observation:run-a",
            "Run metadata field $.patch_artifact_path records local-only artifact path /tmp/run-a/patch.diff; command `tool test`; field `$.patch_artifact_path`",
            AuditFinding::NonPortableArtifactPath,
        );
        let missing_mirror = make_finding_with_kind(
            "artifact_portability",
            "observation:run-b",
            "Run metadata field $.patch_artifact_path records remote artifact ref runner-artifact://lab/run/patch without a mirrored artifact record; command `tool test`; field `$.patch_artifact_path`",
            AuditFinding::NonPortableArtifactPath,
        );

        assert_ne!(
            AuditFinding(&local_path).fingerprint(),
            AuditFinding(&missing_mirror).fingerprint()
        );
    }

    #[test]
    fn file_from_audit_fingerprint_extracts_file_path() {
        assert_eq!(
            file_from_audit_fingerprint("Commands::src/commands/version.rs::NamingMismatch"),
            Some("src/commands/version.rs".to_string())
        );
    }

    #[test]
    fn file_from_audit_fingerprint_handles_nested_paths() {
        assert_eq!(
            file_from_audit_fingerprint(
                "test_coverage::src/core/code_audit/baseline.rs::MissingTestMethod"
            ),
            Some("src/core/code_audit/baseline.rs".to_string())
        );
    }

    #[test]
    fn file_from_audit_fingerprint_returns_none_for_invalid() {
        assert_eq!(file_from_audit_fingerprint("no_separators"), None);
        assert_eq!(file_from_audit_fingerprint("only::one"), None);
    }

    #[test]
    fn save_baseline_scoped_preserves_out_of_scope() {
        let result_initial = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
                make_finding("Flow", "c.php", "Missing method: register"),
            ],
            "scoped_preserve",
        );
        let _ = save_baseline(&result_initial).unwrap();
        let baseline_before = load_baseline(Path::new(&result_initial.source_path)).unwrap();
        assert_eq!(baseline_before.item_count, 3);

        // Scoped update: only a.php changed, finding resolved
        let mut result_scoped = make_result(vec![], "scoped_preserve_update");
        result_scoped.source_path = result_initial.source_path.clone();

        let changed = vec!["a.php".to_string()];
        let _ = save_baseline_scoped(&result_scoped, &changed).unwrap();

        let baseline_after = load_baseline(Path::new(&result_initial.source_path)).unwrap();
        // a.php removed (was in scope, no new findings), b.php + c.php preserved
        assert_eq!(baseline_after.item_count, 2);
        assert!(!baseline_after
            .known_fingerprints
            .iter()
            .any(|fp| fp.contains("a.php")));
        assert!(baseline_after
            .known_fingerprints
            .iter()
            .any(|fp| fp.contains("b.php")));
        assert!(baseline_after
            .known_fingerprints
            .iter()
            .any(|fp| fp.contains("c.php")));

        let _ = std::fs::remove_dir_all(Path::new(&result_initial.source_path));
    }

    #[test]
    fn save_baseline_scoped_adds_new_in_scope() {
        let result_initial = make_result(
            vec![make_finding("Flow", "a.php", "Missing method: execute")],
            "scoped_add",
        );
        let _ = save_baseline(&result_initial).unwrap();

        // Scoped update: b.php is in scope with a new finding
        let mut result_scoped = make_result(
            vec![make_finding("Flow", "b.php", "Missing method: validate")],
            "scoped_add_update",
        );
        result_scoped.source_path = result_initial.source_path.clone();

        let changed = vec!["b.php".to_string()];
        let _ = save_baseline_scoped(&result_scoped, &changed).unwrap();

        let baseline_after = load_baseline(Path::new(&result_initial.source_path)).unwrap();
        // a.php preserved, b.php added
        assert_eq!(baseline_after.item_count, 2);
        assert!(baseline_after
            .known_fingerprints
            .iter()
            .any(|fp| fp.contains("a.php")));
        assert!(baseline_after
            .known_fingerprints
            .iter()
            .any(|fp| fp.contains("b.php")));

        let _ = std::fs::remove_dir_all(Path::new(&result_initial.source_path));
    }
}
