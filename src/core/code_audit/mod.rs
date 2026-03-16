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
pub mod codebase_map;
mod comment_hygiene;
mod compiler_warnings;
pub mod compare;
pub(crate) mod conventions;
pub(crate) mod core_fingerprint;
mod dead_code;
mod discovery;
pub mod docs;
pub mod docs_audit;
mod duplication;
mod findings;
pub mod fingerprint;
pub(crate) mod impact;
pub(crate) mod import_matching;
mod layer_ownership;
pub(crate) mod naming;
pub mod report;
pub mod run;
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
pub use compare::{
    finding_fingerprint, score_delta, weighted_finding_score_with, AuditConvergenceScoring,
};
pub use conventions::{AuditFinding, Convention, Deviation, Language, Outlier};
pub use duplication::DuplicateGroup;
pub use findings::{Finding, Severity};
pub use fingerprint::FileFingerprint;
pub use report::AuditCommandOutput;
pub use run::{run_main_audit_workflow, AuditRunWorkflowArgs, AuditRunWorkflowResult};
pub use walker::is_test_path;

use crate::{component, is_zero, Result};

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
    let comp = component::resolve_effective(Some(component_id), None, None)?;
    component::validate_local_path(&comp)?;
    audit_path_with_id(component_id, &comp.local_path)
}

/// Read reference dependency paths from HOMEBOY_AUDIT_REFERENCE_PATHS env var.
///
/// Reference dependencies are external codebases (e.g. WordPress core, plugin
/// dependencies) whose fingerprints are included in cross-reference analysis
/// (dead code detection) but excluded from convention discovery and duplication
/// detection. This eliminates false positives for functions called via framework
/// hooks, callbacks, or inherited methods.
fn read_reference_paths_from_env() -> Vec<String> {
    std::env::var("HOMEBOY_AUDIT_REFERENCE_PATHS")
        .unwrap_or_default()
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && Path::new(s).is_dir())
        .collect()
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
    let ref_paths = read_reference_paths_from_env();
    audit_internal(component_id, source_path, None, None, &ref_paths)
}

/// Audit only specific files within a component path.
///
/// Used for PR-scoped audits (`--changed-since`) where only changed files
/// should be checked. Conventions are discovered from the full codebase,
/// but findings are scoped to changed files + their affected call sites.
///
/// When `git_ref` is provided, the engine diffs fingerprints of changed files
/// against their base-ref versions to detect symbol changes (renames, removals,
/// signature changes), then fans out to find all files that reference those
/// changed symbols. This catches breakage at call sites, not just in changed files.
pub fn audit_path_scoped(
    component_id: &str,
    source_path: &str,
    file_filter: &[String],
    git_ref: Option<&str>,
) -> Result<CodeAuditResult> {
    let ref_paths = read_reference_paths_from_env();
    audit_internal(component_id, source_path, Some(file_filter), git_ref, &ref_paths)
}

/// Internal audit implementation supporting optional file scoping and impact tracing.
///
/// `reference_paths` are external codebases whose fingerprints are included in
/// cross-reference analysis (dead code) but excluded from convention discovery,
/// duplication detection, and structural analysis.
fn audit_internal(
    component_id: &str,
    source_path: &str,
    file_filter: Option<&[String]>,
    git_ref: Option<&str>,
    reference_paths: &[String],
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

    // Build convention method set ONCE — used by duplication, near-duplicate, and parallel detectors.
    // Convention-expected methods are excluded from duplication/parallel findings because identical
    // or similar implementations across convention-following files are correct behavior.
    let convention_methods = build_convention_method_set(&discovered_conventions, &all_fingerprints);

    let duplication_findings = duplication::detect_duplicates(&all_fingerprints, &convention_methods);
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

    // Phase 4c2: Intra-method duplication (duplicated blocks within a single method)
    let intra_dup_findings = duplication::detect_intra_method_duplicates(&all_fingerprints);
    if !intra_dup_findings.is_empty() {
        log_status!(
            "audit",
            "Intra-method duplication: {} finding(s) (duplicated blocks within methods)",
            intra_dup_findings.len()
        );
        all_findings.extend(intra_dup_findings);
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

    // Phase 4d2: Parallel implementation detection (similar call patterns across files)
    let parallel_findings =
        duplication::detect_parallel_implementations(&all_fingerprints, &convention_methods);
    if !parallel_findings.is_empty() {
        log_status!(
            "audit",
            "Parallel implementations: {} finding(s) (similar call patterns in different functions)",
            parallel_findings.len()
        );
        all_findings.extend(parallel_findings);
    }

    // Phase 4e: Dead code detection (unused params, unreferenced exports, orphaned internals)
    //
    // Reference dependencies (e.g. WordPress core, plugin dependencies) are fingerprinted
    // and included in the cross-reference set so that functions called via framework hooks,
    // callbacks, or inherited methods are recognized as referenced.
    let ref_fingerprints = fingerprint_reference_paths(reference_paths);
    let ref_fp_refs: Vec<&fingerprint::FileFingerprint> = ref_fingerprints.iter().collect();
    let dead_code_findings = dead_code::analyze_dead_code(&all_fingerprints, &ref_fp_refs);
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

    // Phase 4j: Documentation drift detection (broken/stale references in markdown)
    let doc_findings = detect_doc_drift(root, component_id);
    if !doc_findings.is_empty() {
        log_status!(
            "audit",
            "Docs: {} finding(s) (broken references, stale paths)",
            doc_findings.len()
        );
        all_findings.extend(doc_findings);
    }

    // Phase 4l: Compiler warnings (dead code, unused imports, unused variables)
    // Runs cargo check / tsc / go vet and parses warnings into findings.
    let compiler_findings = compiler_warnings::run(root);
    if !compiler_findings.is_empty() {
        log_status!(
            "audit",
            "Compiler warnings: {} finding(s) (dead code, unused imports, unused variables)",
            compiler_findings.len()
        );
        all_findings.extend(compiler_findings);
    }

    // Phase 4m: Impact-scoped filtering — when auditing changed files only,
    // expand scope to include call sites affected by symbol changes, then
    // filter findings to that expanded scope.
    //
    // With git_ref: diff fingerprints against base ref, find affected call sites,
    //   report findings in changed files + affected files.
    // Without git_ref: fall back to simple filename filter (changed files only).
    if let Some(filter) = file_filter {
        let before = all_findings.len();

        let scope_files: std::collections::HashSet<String> = if let Some(ref_str) = git_ref {
            let (expanded_scope, affected) =
                impact::expand_scope(source_path, ref_str, filter, &all_fingerprints);

            if !affected.is_empty() {
                log_status!(
                    "audit",
                    "Impact: {} affected call-site file(s) added to scope",
                    affected.len()
                );
                for af in &affected {
                    let reason_strs: Vec<String> =
                        af.reasons.iter().map(|r| r.to_string()).collect();
                    log_status!(
                        "audit",
                        "  {} → {} ({})",
                        af.source_file,
                        af.file,
                        reason_strs.join(", ")
                    );
                }
            }

            expanded_scope
        } else {
            // No git ref — simple filename filter (legacy behavior)
            filter.iter().cloned().collect()
        };

        all_findings.retain(|f| {
            scope_files
                .iter()
                .any(|scope| f.file.contains(scope.as_str()))
        });
        let filtered_out = before - all_findings.len();
        if filtered_out > 0 {
            log_status!(
                "audit",
                "Scoped: filtered {} finding(s) from out-of-scope files ({} remaining)",
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
// Documentation drift detection
// ============================================================================

/// Detect documentation drift — broken and stale references in markdown files.
///
/// Scans all `.md` files in common docs directories, extracts verifiable claims
/// (file paths, directory paths, class names), and checks each against the
/// codebase. Broken claims become `Finding` entries in the unified audit pipeline.
fn detect_doc_drift(root: &Path, component_id: &str) -> Vec<Finding> {
    use docs_audit::claims::ClaimConfidence;

    let mut findings = Vec::new();

    // Find docs directory
    let docs_dirs = ["docs", "doc", "documentation"];
    let docs_entry = docs_dirs.iter().find_map(|d| {
        let p = root.join(d);
        if p.is_dir() {
            Some((p, *d))
        } else {
            None
        }
    });

    let Some((docs_path, docs_dir_name)) = docs_entry else {
        return findings;
    };

    let doc_excludes = if let Ok(comp) = component::load(component_id) {
        crate::component::scope::resolve_component_scope(
            &comp,
            crate::component::scope::ScopeCommand::Audit,
        )
        .exclude
    } else {
        Vec::new()
    };

    let doc_files = docs_audit::find_doc_files(&docs_path, &doc_excludes);
    if doc_files.is_empty() {
        return findings;
    }

    // Load extension-configured ignore patterns if component is registered
    let ignore_patterns = if let Ok(comp) = component::load(component_id) {
        docs_audit::collect_extension_ignore_patterns(&comp)
    } else {
        Vec::new()
    };

    for relative_doc in &doc_files {
        let abs_doc = docs_path.join(relative_doc);
        let content = match std::fs::read_to_string(&abs_doc) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let finding_file = format!("{}/{}", docs_dir_name, relative_doc);
        let claims = docs_audit::claims::extract_claims(&content, &finding_file, &ignore_patterns);

        for claim in claims {
            // Skip example/placeholder paths — they're illustrative, not real references
            if claim.confidence == ClaimConfidence::Example {
                continue;
            }

            let result = docs_audit::verify::verify_claim(&claim, root, &docs_path, None);

            match result {
                docs_audit::VerifyResult::Broken { suggestion } => {
                    let suggestion_text = suggestion.unwrap_or_default();
                    let (kind, description) = classify_broken_doc_ref(
                        &claim.claim_type,
                        &claim.value,
                        claim.line,
                        &suggestion_text,
                    );

                    findings.push(Finding {
                        convention: "docs".to_string(),
                        severity: match claim.confidence {
                            ClaimConfidence::Real => Severity::Warning,
                            ClaimConfidence::Example | ClaimConfidence::Unclear => Severity::Info,
                        },
                        file: finding_file.clone(),
                        description,
                        suggestion: suggestion_text,
                        kind,
                    });
                }
                docs_audit::VerifyResult::Verified
                | docs_audit::VerifyResult::NeedsVerification { .. } => {}
            }
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });

    findings
}

/// Classify a broken reference as stale (moved target) or truly broken.
fn classify_broken_doc_ref(
    claim_type: &docs_audit::ClaimType,
    value: &str,
    line: usize,
    suggestion: &str,
) -> (AuditFinding, String) {
    let s = suggestion.to_lowercase();
    let label = match claim_type {
        docs_audit::ClaimType::FilePath => "file reference",
        docs_audit::ClaimType::DirectoryPath => "directory reference",
        docs_audit::ClaimType::CodeExample => "code example",
        docs_audit::ClaimType::ClassName => "class reference",
    };

    if s.contains("did you mean")
        || s.contains("moved to")
        || s.contains("similar")
        || s.contains("renamed")
    {
        (
            AuditFinding::StaleDocReference,
            format!(
                "Stale {} `{}` (line {}) — target has moved",
                label, value, line
            ),
        )
    } else {
        (
            AuditFinding::BrokenDocReference,
            format!(
                "Broken {} `{}` (line {}) — target does not exist",
                label, value, line
            ),
        )
    }
}

// ============================================================================
// Reference dependency fingerprinting
// ============================================================================

/// Build the unified convention method set used by duplication and parallel detectors.
///
/// Collects methods from three sources:
/// 1. Per-directory convention expected_methods
/// 2. Cross-directory conventions (methods shared across sibling directory conventions)
/// 3. Cross-file frequency (methods appearing in 3+ files)
/// 4. Naming pattern conventions (prefixes with 5+ unique names across 5+ files)
fn build_convention_method_set(
    discovered_conventions: &[conventions::Convention],
    all_fingerprints: &[&fingerprint::FileFingerprint],
) -> std::collections::HashSet<String> {
    use std::collections::HashMap;

    // 1. Per-directory convention methods
    let mut methods: std::collections::HashSet<String> = discovered_conventions
        .iter()
        .flat_map(|c| c.expected_methods.iter().cloned())
        .collect();

    // 2. Cross-directory: methods shared across 2+ sibling directory conventions
    {
        let mut method_by_parent: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for conv in discovered_conventions {
            let parts: Vec<&str> = conv.glob.split('/').collect();
            if parts.len() >= 3 {
                let parent = parts[..parts.len() - 2].join("/");
                let entry = method_by_parent.entry(parent).or_default();
                for method in &conv.expected_methods {
                    *entry.entry(method.clone()).or_insert(0) += 1;
                }
            }
        }
        for (_parent, parent_methods) in &method_by_parent {
            for (method, count) in parent_methods {
                if *count >= 2 {
                    methods.insert(method.clone());
                }
            }
        }
    }

    // 3. Cross-file frequency: methods appearing in 3+ files
    {
        let mut method_file_count: HashMap<&str, usize> = HashMap::new();
        for fp in all_fingerprints {
            let mut seen_in_file = std::collections::HashSet::new();
            for method in &fp.methods {
                if seen_in_file.insert(method.as_str()) {
                    *method_file_count.entry(method.as_str()).or_insert(0) += 1;
                }
            }
        }
        for (method, count) in &method_file_count {
            if *count >= 3 {
                methods.insert(method.to_string());
            }
        }
    }

    // 4. Naming pattern conventions: prefixes with 5+ unique names across 5+ files
    {
        fn extract_prefix(name: &str) -> Option<&str> {
            if let Some(pos) = name.find(|c: char| c.is_uppercase()) {
                if pos > 0 {
                    return Some(&name[..pos]);
                }
            }
            if let Some(pos) = name.find('_') {
                if pos > 0 {
                    return Some(&name[..pos]);
                }
            }
            None
        }

        let mut prefix_methods: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
        let mut prefix_files: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();

        for fp in all_fingerprints {
            for method in &fp.methods {
                if let Some(prefix) = extract_prefix(method) {
                    prefix_methods
                        .entry(prefix)
                        .or_default()
                        .insert(method.as_str());
                    prefix_files
                        .entry(prefix)
                        .or_default()
                        .insert(fp.relative_path.as_str());
                }
            }
        }

        for (prefix, prefix_method_set) in &prefix_methods {
            let file_count = prefix_files.get(prefix).map(|f| f.len()).unwrap_or(0);
            if prefix_method_set.len() >= 5 && file_count >= 5 {
                for method in prefix_method_set {
                    methods.insert(method.to_string());
                }
            }
        }
    }

    methods
}

/// Fingerprint external reference paths for cross-reference analysis.
///
/// Walks each reference path and fingerprints all source files found.
/// These fingerprints provide the call/import data that dead code detection
/// uses to determine whether a function is referenced externally (e.g. by
/// WordPress core calling a hook callback, or a parent plugin importing a class).
///
/// Reference fingerprints are NOT used for convention discovery, duplication
/// detection, or structural analysis — they only enrich the cross-reference set.
fn fingerprint_reference_paths(reference_paths: &[String]) -> Vec<fingerprint::FileFingerprint> {
    if reference_paths.is_empty() {
        return Vec::new();
    }

    let mut ref_fps = Vec::new();
    let mut total_files = 0;

    for ref_path in reference_paths {
        let root = Path::new(ref_path);
        if !root.is_dir() {
            continue;
        }

        if let Ok(walker_iter) = walker::walk_source_files(root) {
            for path in walker_iter {
                if let Some(fp) = fingerprint::fingerprint_file(&path, root) {
                    ref_fps.push(fp);
                    total_files += 1;
                }
            }
        }
    }

    if total_files > 0 {
        log_status!(
            "audit",
            "Reference dependencies: {} file(s) fingerprinted from {} path(s)",
            total_files,
            reference_paths.len()
        );
    }

    ref_fps
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
        let _ = fs::create_dir_all(dir.join("inc/Core/Steps"));

        fs::write(
            dir.join("homeboy.json"),
            r#"{
              "audit_rules": {
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
              }
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
