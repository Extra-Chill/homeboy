//! Baseline management for CI-friendly audit drift detection.
//!
//! Saves a snapshot of audit state and compares future runs against it.
//! Only NEW drift (findings not in the baseline) triggers a failure.

use std::collections::HashSet;
use std::path::Path;

use super::CodeAuditResult;

/// A saved baseline snapshot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditBaseline {
    /// ISO 8601 timestamp when the baseline was created.
    pub created_at: String,
    /// Component that was audited.
    pub component_id: String,
    /// Total findings at baseline time.
    pub findings_count: usize,
    /// Total outlier files at baseline time.
    pub outliers_count: usize,
    /// Alignment score at baseline time.
    pub alignment_score: Option<f32>,
    /// Set of known outlier file paths (accepted drift).
    pub known_outliers: Vec<String>,
    /// Fingerprint of each known finding: "convention::file::kind::description"
    pub known_findings: Vec<String>,
}

/// Result of comparing an audit against a baseline.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BaselineComparison {
    /// Findings that are new since the baseline.
    pub new_findings: Vec<NewFinding>,
    /// Findings that were in the baseline but are now resolved.
    pub resolved_findings: Vec<String>,
    /// Net change in findings count.
    pub delta: i64,
    /// Whether drift increased (true = fail in CI).
    pub drift_increased: bool,
}

/// A finding that wasn't in the baseline.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NewFinding {
    /// The finding fingerprint.
    pub fingerprint: String,
    /// Human-readable description.
    pub description: String,
    /// The file involved.
    pub file: String,
    /// The convention involved.
    pub convention: String,
}

// ============================================================================
// Baseline file path
// ============================================================================

const BASELINE_DIR: &str = ".homeboy";
const BASELINE_FILE: &str = "audit-baseline.json";

/// Get the baseline file path for a source directory.
pub fn baseline_path(source_path: &Path) -> std::path::PathBuf {
    source_path.join(BASELINE_DIR).join(BASELINE_FILE)
}

// ============================================================================
// Save baseline
// ============================================================================

/// Save the current audit result as a baseline.
pub fn save_baseline(result: &CodeAuditResult) -> Result<std::path::PathBuf, String> {
    let source = Path::new(&result.source_path);
    let dir = source.join(BASELINE_DIR);

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;

    let known_outliers: Vec<String> = result
        .conventions
        .iter()
        .flat_map(|c| c.outliers.iter().map(|o| o.file.clone()))
        .collect();

    let known_findings: Vec<String> = result
        .findings
        .iter()
        .map(|f| {
            finding_fingerprint(
                &f.convention,
                &f.file,
                &format!("{:?}", f.kind),
                &f.description,
            )
        })
        .collect();

    let baseline = AuditBaseline {
        created_at: chrono_now(),
        component_id: result.component_id.clone(),
        findings_count: result.findings.len(),
        outliers_count: known_outliers.len(),
        alignment_score: result.summary.alignment_score,
        known_outliers,
        known_findings,
    };

    let path = baseline_path(source);
    let json = serde_json::to_string_pretty(&baseline)
        .map_err(|e| format!("Failed to serialize baseline: {}", e))?;

    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;

    Ok(path)
}

// ============================================================================
// Load baseline
// ============================================================================

/// Load a baseline if one exists for the given source path.
pub fn load_baseline(source_path: &Path) -> Option<AuditBaseline> {
    let path = baseline_path(source_path);
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

// ============================================================================
// Compare against baseline
// ============================================================================

/// Compare an audit result against a saved baseline.
pub fn compare(result: &CodeAuditResult, baseline: &AuditBaseline) -> BaselineComparison {
    let current_fingerprints: HashSet<String> = result
        .findings
        .iter()
        .map(|f| {
            finding_fingerprint(
                &f.convention,
                &f.file,
                &format!("{:?}", f.kind),
                &f.description,
            )
        })
        .collect();

    let baseline_fingerprints: HashSet<String> = baseline.known_findings.iter().cloned().collect();

    // New = in current but not in baseline
    let new_findings: Vec<NewFinding> = result
        .findings
        .iter()
        .filter(|f| {
            let fp = finding_fingerprint(
                &f.convention,
                &f.file,
                &format!("{:?}", f.kind),
                &f.description,
            );
            !baseline_fingerprints.contains(&fp)
        })
        .map(|f| NewFinding {
            fingerprint: finding_fingerprint(
                &f.convention,
                &f.file,
                &format!("{:?}", f.kind),
                &f.description,
            ),
            description: f.description.clone(),
            file: f.file.clone(),
            convention: f.convention.clone(),
        })
        .collect();

    // Resolved = in baseline but not in current
    let resolved_findings: Vec<String> = baseline_fingerprints
        .difference(&current_fingerprints)
        .cloned()
        .collect();

    let delta = result.findings.len() as i64 - baseline.findings_count as i64;
    let drift_increased = !new_findings.is_empty();

    BaselineComparison {
        new_findings,
        resolved_findings,
        delta,
        drift_increased,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Create a stable fingerprint for a finding.
///
/// Uses `convention::file::kind` as the core identity. The description is
/// excluded because structural findings embed volatile values (e.g. exact
/// line counts) that change when a file grows by even one line. Including
/// them would cause the same finding to appear as "resolved + new" on every
/// minor change, defeating the baseline ratchet.
fn finding_fingerprint(convention: &str, file: &str, kind: &str, _description: &str) -> String {
    format!("{}::{}::{}", convention, file, kind)
}

/// Get current UTC timestamp as ISO 8601.
fn chrono_now() -> String {
    // Use std::time since we don't want a chrono dependency
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple ISO 8601 from unix timestamp
    // 2026-02-27T21:00:00Z format
    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;

    let days = now / secs_per_day;
    let remaining = now % secs_per_day;
    let hours = remaining / secs_per_hour;
    let remaining = remaining % secs_per_hour;
    let minutes = remaining / secs_per_min;
    let seconds = remaining % secs_per_min;

    // Calculate date from days since epoch (1970-01-01)
    let (year, month, day) = days_to_date(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap_year(year);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::DeviationKind;
    use crate::code_audit::findings::{Finding, Severity};
    use crate::code_audit::{AuditSummary, CodeAuditResult};

    fn make_finding(convention: &str, file: &str, description: &str) -> Finding {
        Finding {
            convention: convention.to_string(),
            severity: Severity::Warning,
            file: file.to_string(),
            description: description.to_string(),
            suggestion: String::new(),
            kind: DeviationKind::MissingMethod,
        }
    }

    fn make_result(findings: Vec<Finding>, test_name: &str) -> CodeAuditResult {
        let dir = std::env::temp_dir().join(format!("homeboy_baseline_{}", test_name));
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
        assert_eq!(loaded.component_id, "test");
        assert_eq!(loaded.findings_count, 2);
        assert_eq!(loaded.known_findings.len(), 2);

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
        assert!(comparison.new_findings.is_empty());
        assert!(comparison.resolved_findings.is_empty());
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
        assert_eq!(comparison.new_findings.len(), 1);
        assert_eq!(comparison.new_findings[0].file, "c.php");
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
        assert!(comparison.new_findings.is_empty());
        assert_eq!(comparison.resolved_findings.len(), 1);
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
        assert_eq!(comparison.new_findings.len(), 1);
        assert_eq!(comparison.resolved_findings.len(), 1);
        assert_eq!(comparison.delta, 0);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn no_baseline_returns_none() {
        let result = load_baseline(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }

    #[test]
    fn finding_fingerprint_is_stable() {
        let fp1 = finding_fingerprint("Flow", "a.php", "MissingMethod", "Missing method: execute");
        let fp2 = finding_fingerprint("Flow", "a.php", "MissingMethod", "Missing method: execute");
        assert_eq!(fp1, fp2);

        let fp3 = finding_fingerprint("Flow", "b.php", "MissingMethod", "Missing method: execute");
        assert_ne!(fp1, fp3);
    }

    #[test]
    fn finding_fingerprint_ignores_description() {
        // Structural findings embed volatile values (line counts, item counts)
        // in the description. The fingerprint must be stable across these changes.
        let fp1 = finding_fingerprint(
            "structural",
            "deploy.rs",
            "GodFile",
            "File has 2484 lines (threshold: 500)",
        );
        let fp2 = finding_fingerprint(
            "structural",
            "deploy.rs",
            "GodFile",
            "File has 2645 lines (threshold: 500)",
        );
        assert_eq!(
            fp1, fp2,
            "fingerprint should not change when line count changes"
        );
    }

    #[test]
    fn chrono_now_produces_valid_iso8601() {
        let now = chrono_now();
        // Should match YYYY-MM-DDTHH:MM:SSZ
        assert!(
            now.len() == 20,
            "Expected 20 chars, got {}: {}",
            now.len(),
            now
        );
        assert!(now.ends_with('Z'));
        assert!(now.contains('T'));
    }
}
