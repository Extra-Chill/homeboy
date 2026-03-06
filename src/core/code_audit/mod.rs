//! Code audit system for convention detection, drift analysis, and structural complexity.
//!
//! Scans source code to discover structural conventions, detect outliers,
//! report architectural drift, and flag structural issues (god files, high item counts).
//! Works by:
//!
//! 1. Fingerprinting source files (extract methods, registrations, types)
//! 2. Grouping files by directory and language
//! 3. Discovering conventions (patterns most files follow)
//! 4. Checking all files against discovered conventions
//! 5. Producing actionable findings for outliers
//! 6. Analyzing structural complexity (god files, high item counts)

pub mod baseline;
mod checks;
mod comment_hygiene;
pub(crate) mod conventions;
mod dead_code;
mod discovery;
mod duplication;
mod findings;
pub mod fingerprint;
pub mod fixer;
pub(crate) mod import_matching;
mod layer_ownership;
pub(crate) mod preflight;
mod signatures;
mod structural;
mod test_coverage;
pub(crate) mod test_mapping;
mod test_topology;
pub(crate) mod walker;

#[cfg(test)]
pub(crate) mod test_helpers;

use std::path::Path;

use self::layer_ownership::run as run_layer_ownership;

pub use checks::{CheckResult, CheckStatus};
pub use conventions::{Convention, Deviation, DeviationKind, Language, Outlier};
pub use findings::{Finding, Severity};
pub use fingerprint::FileFingerprint;

use crate::{component, utils::is_zero, Result};

/// Summary counts for the audit report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditSummary {
    pub files_scanned: usize,
    pub conventions_detected: usize,
    #[serde(skip_serializing_if = "is_zero", default)]
    pub outliers_found: usize,
    /// Overall alignment score (0.0 = total chaos, 1.0 = perfect consistency).
    /// Null when no files could be fingerprinted (score would be meaningless).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub alignment_score: Option<f32>,
    /// Source files found but not fingerprinted (no extension provides fingerprinting).
    #[serde(skip_serializing_if = "is_zero", default)]
    pub files_skipped: usize,
    /// Warnings about the audit (e.g., unsupported file types).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Complete result of auditing a component's code conventions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeAuditResult {
    pub component_id: String,
    pub source_path: String,
    pub summary: AuditSummary,
    pub conventions: Vec<ConventionReport>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub directory_conventions: Vec<DirectoryConvention>,
    pub findings: Vec<Finding>,
    /// Grouped duplications for the fixer — each group has a canonical file and removal targets.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub duplicate_groups: Vec<duplication::DuplicateGroup>,
}

/// A cross-directory convention: a pattern that sibling subdirectories share.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectoryConvention {
    /// Parent directory path (e.g., "inc/Abilities").
    pub parent: String,
    /// Expected methods that most subdirectories' conventions share.
    pub expected_methods: Vec<String>,
    /// Expected registrations that most subdirectories share.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_registrations: Vec<String>,
    /// Subdirectories that conform.
    pub conforming_dirs: Vec<String>,
    /// Subdirectories that deviate.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub outlier_dirs: Vec<DirectoryOutlier>,
    /// How many subdirectories were analyzed.
    pub total_dirs: usize,
    /// Confidence score.
    pub confidence: f32,
}

/// A subdirectory that deviates from the cross-directory convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectoryOutlier {
    /// Subdirectory name.
    pub dir: String,
    /// What's missing compared to sibling conventions.
    pub missing_methods: Vec<String>,
    /// Missing registrations.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub missing_registrations: Vec<String>,
}

/// A convention as reported to the user (includes check status).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConventionReport {
    pub name: String,
    pub glob: String,
    pub status: CheckStatus,
    pub expected_methods: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_registrations: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_interfaces: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_namespace: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_imports: Vec<String>,
    pub conforming: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub outliers: Vec<Outlier>,
    pub total_files: usize,
    pub confidence: f32,
}

// ============================================================================
// Public API
// ============================================================================

/// Audit a registered component by ID.
pub fn audit_component(component_id: &str) -> Result<CodeAuditResult> {
    let comp = component::load(component_id)?;
    component::validate_local_path(&comp)?;
    audit_path_with_id(component_id, &comp.local_path)
}

/// Audit a filesystem path directly (no registered component needed).
pub fn audit_path(path: &str) -> Result<CodeAuditResult> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(crate::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {}", path),
            None,
            None,
        ));
    }

    // Use directory name as component_id
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    audit_path_with_id(&name, path)
}

/// Core audit logic shared by both entry points.
/// Also available for callers that have a component ID and an overridden path.
pub fn audit_path_with_id(component_id: &str, source_path: &str) -> Result<CodeAuditResult> {
    audit_internal(component_id, source_path, None)
}

/// Audit only specific files within a component path.
///
/// Used for PR-scoped audits (`--changed-since`) where only changed files
/// should be checked. Uses the same conventions and checks as a full audit
/// but limits file walking to the provided filter.
pub fn audit_path_scoped(
    component_id: &str,
    source_path: &str,
    file_filter: &[String],
) -> Result<CodeAuditResult> {
    audit_internal(component_id, source_path, Some(file_filter))
}

/// Internal audit implementation supporting optional file scoping.
fn audit_internal(
    component_id: &str,
    source_path: &str,
    file_filter: Option<&[String]>,
) -> Result<CodeAuditResult> {
    let root = Path::new(source_path);

    if let Some(filter) = file_filter {
        log_status!(
            "audit",
            "Scanning {} changed file(s) in {} for conventions...",
            filter.len(),
            source_path
        );
    } else {
        log_status!("audit", "Scanning {} for conventions...", source_path);
    }

    // Phase 1: Auto-discover file groups (always full codebase for convention detection)
    let discovery = discovery::auto_discover_groups(root);
    let files_skipped = discovery
        .files_walked
        .saturating_sub(discovery.files_fingerprinted);

    if discovery.groups.is_empty() {
        let mut warnings = Vec::new();
        let unclaimed = walker::count_unclaimed_source_files(root);
        let total_skipped = files_skipped + unclaimed;

        if unclaimed > 0 {
            warnings.push(format!(
                "Found {} source file(s) but no installed extension provides fingerprinting for these file types. \
                 Install or update an extension with a `provides.file_extensions` and `scripts.fingerprint` config.",
                unclaimed
            ));
            log_status!(
                "audit",
                "WARNING: {} source files found but none could be fingerprinted (no extension claims these file types)",
                unclaimed
            );
        } else if discovery.files_walked > 0 && discovery.files_fingerprinted == 0 {
            warnings.push(format!(
                "Found {} source file(s) but no extension could fingerprint them.",
                discovery.files_walked
            ));
            log_status!(
                "audit",
                "WARNING: {} source files found but none could be fingerprinted",
                discovery.files_walked
            );
        } else {
            log_status!("audit", "No source files found");
        }
        return Ok(CodeAuditResult {
            component_id: component_id.to_string(),
            source_path: source_path.to_string(),
            summary: AuditSummary {
                files_scanned: 0,
                conventions_detected: 0,
                outliers_found: 0,
                alignment_score: None,
                files_skipped: total_skipped,
                warnings,
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![],
            duplicate_groups: vec![],
        });
    }

    // Phase 2: Discover conventions for each group
    let mut discovered_conventions = Vec::new();
    let mut total_files = 0;

    for (name, glob, fingerprints) in &discovery.groups {
        total_files += fingerprints.len();
        if let Some(convention) = conventions::discover_conventions(name, glob, fingerprints) {
            discovered_conventions.push(convention);
        }
    }

    // Phase 2b: Check signature consistency within conventions
    conventions::check_signature_consistency(&mut discovered_conventions, root);

    // Phase 3: Check all conventions
    let check_results = checks::check_conventions(&discovered_conventions);

    // Phase 4: Build findings
    let mut all_findings = findings::build_findings(&check_results);

    // Phase 4b: Structural complexity analysis (god files, high item counts)
    let structural_findings = structural::analyze_structure(root);
    if !structural_findings.is_empty() {
        log_status!(
            "audit",
            "Structural: {} finding(s) (god files, high item counts)",
            structural_findings.len()
        );
        all_findings.extend(structural_findings);
    }

    // Phase 4c: Duplication detection (identical function bodies across files)
    let all_fingerprints: Vec<&fingerprint::FileFingerprint> = discovery
        .groups
        .iter()
        .flat_map(|(_, _, fps)| fps.iter())
        .collect();
    let duplication_findings = duplication::detect_duplicates(&all_fingerprints);
    let duplicate_groups = duplication::detect_duplicate_groups(&all_fingerprints);
    if !duplication_findings.is_empty() {
        log_status!(
            "audit",
            "Duplication: {} finding(s) across {} group(s)",
            duplication_findings.len(),
            duplicate_groups.len()
        );
        all_findings.extend(duplication_findings);
    }

    // Phase 4d: Near-duplicate detection (structural similarity)
    let near_dup_findings = duplication::detect_near_duplicates(&all_fingerprints);
    if !near_dup_findings.is_empty() {
        log_status!(
            "audit",
            "Near-duplicates: {} finding(s) (structural matches with different identifiers)",
            near_dup_findings.len()
        );
        all_findings.extend(near_dup_findings);
    }

    // Phase 4e: Dead code detection (unused params, unreferenced exports, orphaned internals)
    let dead_code_findings = dead_code::analyze_dead_code(&all_fingerprints);
    if !dead_code_findings.is_empty() {
        log_status!(
            "audit",
            "Dead code: {} finding(s) (unused params, unreferenced exports, orphaned internals)",
            dead_code_findings.len()
        );
        all_findings.extend(dead_code_findings);
    }

    // Phase 4f: Comment hygiene detection (TODO/FIXME/HACK + stale phrasing)
    let comment_findings = comment_hygiene::run(&all_fingerprints);
    if !comment_findings.is_empty() {
        log_status!(
            "audit",
            "Comment hygiene: {} finding(s) (TODO/FIXME/HACK markers, stale phrasing)",
            comment_findings.len()
        );
        all_findings.extend(comment_findings);
    }

    // Phase 4g: Structural test coverage gap detection
    // Look up the extension's test mapping config for the component.
    if let Ok(comp) = component::load(component_id) {
        if let Some(extensions) = &comp.extensions {
            for ext_id in extensions.keys() {
                if let Ok(ext_manifest) = crate::extension::load_extension(ext_id) {
                    if let Some(test_mapping) = ext_manifest.test_mapping() {
                        let coverage_findings = test_coverage::analyze_test_coverage(
                            root,
                            &all_fingerprints,
                            test_mapping,
                        );
                        if !coverage_findings.is_empty() {
                            log_status!(
                                "audit",
                                "Test coverage: {} finding(s) (missing test files, uncovered methods, orphaned tests)",
                                coverage_findings.len()
                            );
                            all_findings.extend(coverage_findings);
                        }
                        break; // Only use the first extension that has test_mapping
                    }
                }
            }
        }
    }

    // Phase 4h: Architecture/layer ownership rule checks (optional config)
    let layer_findings = run_layer_ownership(root);
    if !layer_findings.is_empty() {
        log_status!(
            "audit",
            "Layer ownership: {} finding(s) (architecture ownership violations)",
            layer_findings.len()
        );
        all_findings.extend(layer_findings);
    }

    // Phase 4i: Test topology checks (extension-driven classification + central policy)
    let topology_findings = test_topology::run(root);
    if !topology_findings.is_empty() {
        log_status!(
            "audit",
            "Test topology: {} finding(s) (inline/scattered test placement)",
            topology_findings.len()
        );
        all_findings.extend(topology_findings);
    }

    // Phase 4j: Scope filtering — when auditing changed files only, remove
    // findings for files that weren't changed. Conventions are still discovered
    // from the full codebase so drift detection is accurate.
    if let Some(filter) = file_filter {
        let before = all_findings.len();
        all_findings.retain(|f| filter.iter().any(|changed| f.file.contains(changed)));
        let filtered_out = before - all_findings.len();
        if filtered_out > 0 {
            log_status!(
                "audit",
                "Scoped: filtered {} finding(s) from unchanged files ({} remaining)",
                filtered_out,
                all_findings.len()
            );
        }
    }

    // Phase 5: Build report
    let total_outliers: usize = discovered_conventions
        .iter()
        .map(|c| c.outliers.len())
        .sum();
    let total_conforming: usize = discovered_conventions
        .iter()
        .map(|c| c.conforming.len())
        .sum();
    let total_in_conventions = total_conforming + total_outliers;
    let alignment_score = if total_in_conventions > 0 {
        Some(total_conforming as f32 / total_in_conventions as f32)
    } else {
        None
    };

    let mut warnings = Vec::new();
    if files_skipped > 0 {
        warnings.push(format!(
            "{} source file(s) found but could not be fingerprinted (no extension provides fingerprinting for these file types)",
            files_skipped
        ));
    }

    let convention_reports: Vec<ConventionReport> = discovered_conventions
        .iter()
        .zip(check_results.iter())
        .map(|(conv, check)| ConventionReport {
            name: conv.name.clone(),
            glob: conv.glob.clone(),
            status: check.status.clone(),
            expected_methods: conv.expected_methods.clone(),
            expected_registrations: conv.expected_registrations.clone(),
            expected_interfaces: conv.expected_interfaces.clone(),
            expected_namespace: conv.expected_namespace.clone(),
            expected_imports: conv.expected_imports.clone(),
            conforming: conv.conforming.clone(),
            outliers: conv.outliers.clone(),
            total_files: conv.total_files,
            confidence: conv.confidence,
        })
        .collect();

    log_status!(
        "audit",
        "Complete: {} files, {} conventions, {} outliers (alignment: {:.0}%)",
        total_files,
        convention_reports.len(),
        total_outliers,
        alignment_score.unwrap_or(0.0) * 100.0
    );

    // Phase 6: Cross-directory convention discovery
    let directory_conventions = discovery::discover_cross_directory(&convention_reports);

    if !directory_conventions.is_empty() {
        let total_dir_outliers: usize = directory_conventions
            .iter()
            .map(|d| d.outlier_dirs.len())
            .sum();
        log_status!(
            "audit",
            "Cross-directory: {} pattern(s), {} outlier dir(s)",
            directory_conventions.len(),
            total_dir_outliers
        );
    }

    Ok(CodeAuditResult {
        component_id: component_id.to_string(),
        source_path: source_path.to_string(),
        summary: AuditSummary {
            files_scanned: total_files,
            conventions_detected: convention_reports.len(),
            outliers_found: total_outliers,
            alignment_score,
            files_skipped,
            warnings,
        },
        conventions: convention_reports,
        directory_conventions,
        findings: all_findings,
        duplicate_groups,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn audit_nonexistent_path_returns_error() {
        let result = audit_path("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }

    #[test]
    fn audit_empty_directory_returns_clean() {
        let dir = std::env::temp_dir().join("homeboy_audit_test_empty");
        let _ = fs::create_dir_all(&dir);

        let result = audit_path(dir.to_str().unwrap()).unwrap();
        assert_eq!(result.summary.files_scanned, 0);
        assert!(result.summary.alignment_score.is_none());
        assert!(result.conventions.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_analyze_layer_ownership() {
        let dir = std::env::temp_dir().join("homeboy_audit_layer_test");
        let _ = fs::create_dir_all(dir.join(".homeboy"));
        let _ = fs::create_dir_all(dir.join("inc/Core/Steps"));

        fs::write(
            dir.join(".homeboy/audit-rules.json"),
            r#"{
              "layer_rules": [
                {
                  "name": "engine-owns-terminal-status",
                  "forbid": {
                    "glob": "inc/Core/Steps/**/*.php",
                    "patterns": ["JobStatus::"]
                  },
                  "allow": {"glob": "inc/Abilities/Engine/**/*.php"}
                }
              ]
            }"#,
        )
        .unwrap();

        fs::write(
            dir.join("inc/Core/Steps/agent_ping.php"),
            "<?php\n$status = JobStatus::FAILED;\n",
        )
        .unwrap();

        let findings = layer_ownership::run(&dir);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].convention, "layer_ownership");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "Requires PHP extension with fingerprint script installed"]
    fn audit_directory_with_convention() {
        let dir = std::env::temp_dir().join("homeboy_audit_test_conv");
        let steps = dir.join("steps");
        let _ = fs::create_dir_all(&steps);

        // Create 3 files: 2 follow pattern, 1 is an outlier
        fs::write(
            steps.join("step_a.php"),
            r#"<?php
class StepA {
    public function register() {}
    public function validate($input) {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        fs::write(
            steps.join("step_b.php"),
            r#"<?php
class StepB {
    public function register() {}
    public function validate($input) {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        fs::write(
            steps.join("step_c.php"),
            r#"<?php
class StepC {
    public function register() {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        let result = audit_path(dir.to_str().unwrap()).unwrap();

        assert_eq!(result.summary.files_scanned, 3);
        assert!(result.summary.conventions_detected >= 1);
        assert!(result.summary.outliers_found >= 1);
        assert!(result.summary.alignment_score.unwrap() < 1.0);

        // Find the steps convention
        let steps_conv = result
            .conventions
            .iter()
            .find(|c| c.name == "Steps")
            .expect("Should find Steps convention");

        assert_eq!(steps_conv.total_files, 3);
        assert!(steps_conv
            .expected_methods
            .contains(&"register".to_string()));
        assert!(steps_conv.expected_methods.contains(&"execute".to_string()));
        assert_eq!(steps_conv.outliers.len(), 1);
        assert!(steps_conv.outliers[0].file.contains("step_c"));

        // Should have findings for the outlier
        assert!(!result.findings.is_empty());
        assert!(result
            .findings
            .iter()
            .any(|f| f.file.contains("step_c") && f.description.contains("validate")));

        let _ = fs::remove_dir_all(&dir);
    }
}
