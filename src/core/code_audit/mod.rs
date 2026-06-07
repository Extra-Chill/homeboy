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
mod comment_blocks;
mod comment_hygiene;
pub mod compare;
mod compiler_warnings;
mod convention_membership;
pub(crate) mod conventions;
pub(crate) mod core_fingerprint;
mod dead_code;
mod descriptor_runtime;
mod detectors;
mod discovery;
pub mod docs_audit;
mod duplication;
mod execution_plan;
mod findings;
pub mod fingerprint;
mod idiomatic;
pub(crate) mod impact;
pub(crate) mod import_matching;
pub(crate) mod naming;
pub mod report;
mod requirements;
pub mod run;
mod shadow_modules;
mod signatures;
mod source_locations;
mod structural;
pub(crate) mod test_mapping;
pub(crate) mod walker;

#[cfg(test)]
pub(crate) mod test_helpers;

use std::path::Path;
use std::time::Duration;

use self::detectors::layer_ownership::run as run_layer_ownership;
use self::detectors::{
    artifact_portability, command_status_contracts, config_key_usage, core_boundary_leak,
    dead_guard, deprecation_age, enum_dispatch_contracts, field_patterns, global_env_guard,
    mutating_resource_access, parallel_runner_setup, public_registry_exposure, redirect_validation,
    remote_execution_preflight, requested_detectors, source_policy, test_coverage,
    unbounded_output_capture, wrapper_inference,
};
use descriptor_runtime::{run_descriptor_detectors, DetectorRunContext};

pub use checks::{CheckResult, CheckStatus};
pub use compare::{
    finding_fingerprint, score_delta, weighted_finding_score_with, AuditConvergenceScoring,
};
pub use conventions::{AuditFinding, Convention, Deviation, Language, Outlier};
pub use duplication::DuplicateGroup;
pub(crate) use execution_plan::{
    AuditExecutionPlan, DetectorDescriptor, DetectorRuntime, FingerprintDetectorRunner,
    RootDetectorRunner,
};
pub use findings::{homeboy_finding_from_audit, Finding, FindingConfidence, Severity};
pub use fingerprint::FileFingerprint;
pub use report::AuditCommandOutput;
pub use run::{run_main_audit_workflow, AuditRunWorkflowArgs, AuditRunWorkflowResult};
pub use walker::is_test_path;

use crate::core::component::AuditConfig;
use crate::core::{component, Result};
use crate::is_zero;

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

/// Shared analysis state built during an audit run and reused by downstream
/// consumers that would otherwise re-walk and re-fingerprint the codebase.
#[derive(Debug, Clone, Default)]
pub(crate) struct AuditAnalysisContext {
    pub(crate) fingerprints: Vec<fingerprint::FileFingerprint>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuditWithAnalysis {
    pub(crate) result: CodeAuditResult,
    pub(crate) analysis: AuditAnalysisContext,
    pub timing: AuditTiming,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AuditTiming {
    pub spans: Vec<AuditTimingSpan>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AuditTimingSpan {
    pub id: String,
    pub status: String,
    pub duration_ms: Option<f64>,
}

impl AuditTiming {
    fn push_ok(&mut self, id: impl Into<String>, duration: Duration) {
        self.spans.push(AuditTimingSpan {
            id: id.into(),
            status: "ok".to_string(),
            duration_ms: Some(duration.as_secs_f64() * 1000.0),
        });
    }

    fn push_skipped(&mut self, id: impl Into<String>) {
        self.spans.push(AuditTimingSpan {
            id: id.into(),
            status: "skipped".to_string(),
            duration_ms: None,
        });
    }
}

fn time_audit_detector<T>(
    timing: &mut AuditTiming,
    id: &'static str,
    enabled: bool,
    run: impl FnOnce() -> T,
    skipped: impl FnOnce() -> T,
) -> T {
    if enabled {
        eprintln!("[audit] Running {id}...");
        let started = std::time::Instant::now();
        let value = run();
        let elapsed = started.elapsed();
        eprintln!(
            "[audit] Completed {id} in {:.0}ms",
            elapsed.as_secs_f64() * 1000.0
        );
        timing.push_ok(id, elapsed);
        value
    } else {
        timing.push_skipped(id);
        skipped()
    }
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
        return Err(crate::core::Error::validation_invalid_argument(
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
    audit_internal(
        component_id,
        source_path,
        None,
        None,
        &ref_paths,
        &AuditExecutionPlan::full(),
        &[],
    )
    .map(|audit| audit.result)
}

/// Run only configured source policies for a component path.
pub fn source_policy_findings_for_path(
    component_id: &str,
    source_path: &str,
) -> Result<Vec<Finding>> {
    let root = Path::new(source_path);
    if !root.is_dir() {
        return Err(crate::core::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {source_path}"),
            None,
            None,
        ));
    }

    let audit_config = audit_config_for(component_id, root, &[]);
    let snapshot = walker::walk_all_source_files_snapshot(root);
    let fingerprints = snapshot
        .iter()
        .filter_map(|(path, content)| fingerprint::fingerprint_content(path, root, content))
        .collect::<Vec<_>>();
    let fingerprint_refs = fingerprints.iter().collect::<Vec<_>>();

    Ok(source_policy::run(
        &fingerprint_refs,
        &audit_config.source_policies,
    ))
}

pub(crate) fn audit_path_with_id_with_plan_and_analysis(
    component_id: &str,
    source_path: &str,
    plan: &AuditExecutionPlan,
    reference_paths: &[String],
    extension_overrides: &[String],
) -> Result<AuditWithAnalysis> {
    audit_internal(
        component_id,
        source_path,
        None,
        None,
        reference_paths,
        plan,
        extension_overrides,
    )
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
    audit_internal(
        component_id,
        source_path,
        Some(file_filter),
        git_ref,
        &ref_paths,
        &AuditExecutionPlan::full(),
        &[],
    )
    .map(|audit| audit.result)
}

pub(crate) fn audit_path_scoped_with_plan_and_analysis(
    component_id: &str,
    source_path: &str,
    file_filter: &[String],
    git_ref: Option<&str>,
    plan: &AuditExecutionPlan,
    reference_paths: &[String],
    extension_overrides: &[String],
) -> Result<AuditWithAnalysis> {
    audit_internal(
        component_id,
        source_path,
        Some(file_filter),
        git_ref,
        reference_paths,
        plan,
        extension_overrides,
    )
}

fn audit_config_for(
    component_id: &str,
    root: &Path,
    extension_overrides: &[String],
) -> AuditConfig {
    let component =
        component::discover_from_portable(root).or_else(|| component::load(component_id).ok());
    let mut audit_config = AuditConfig::default();

    if let Some(component) = &component {
        if let Some(extensions) = &component.extensions {
            for extension_id in extensions.keys() {
                if let Ok(manifest) = crate::core::extension::load_extension(extension_id) {
                    if let Some(rules) = manifest.audit_detector_rules() {
                        audit_config.merge(rules);
                    }
                }
            }
        }

        if let Some(component_rules) = &component.audit {
            audit_config.merge(component_rules);
        }
    }

    for extension_id in extension_overrides {
        if let Ok(manifest) = crate::core::extension::load_extension(extension_id) {
            if let Some(rules) = manifest.audit_detector_rules() {
                audit_config.merge(rules);
            }
        }
    }

    audit_config
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
    plan: &AuditExecutionPlan,
    extension_overrides: &[String],
) -> Result<AuditWithAnalysis> {
    let root = Path::new(source_path);
    let audit_config = audit_config_for(component_id, root, extension_overrides);
    let mut timing = AuditTiming::default();

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

    if !plan.requires_discovery() {
        let detector_started = std::time::Instant::now();
        let result = audit_root_only(
            component_id,
            source_path,
            root,
            plan,
            extension_overrides,
            &mut timing,
        );
        timing.push_ok("detectors", detector_started.elapsed());
        return Ok(AuditWithAnalysis {
            result,
            analysis: AuditAnalysisContext::default(),
            timing,
        });
    }

    // Phase 1: Auto-discover file groups (always full codebase for convention detection)
    let discovery_started = std::time::Instant::now();
    let source_snapshot =
        walker::walk_shared_audit_files_snapshot(root, structural::source_extensions());
    let discovery =
        discovery::auto_discover_groups_from_snapshot(root, &audit_config, &source_snapshot);
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
        timing.push_ok("discovery_fingerprinting", discovery_started.elapsed());
        return Ok(AuditWithAnalysis {
            result: CodeAuditResult {
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
            },
            analysis: AuditAnalysisContext::default(),
            timing,
        });
    }

    // Phase 2: Discover conventions for each group
    let mut discovered_conventions = Vec::new();
    let mut total_files = 0;

    for (name, glob, fingerprints) in &discovery.groups {
        total_files += fingerprints.len();
        if let Some(convention) =
            conventions::discover_conventions_with_config(name, glob, fingerprints, &audit_config)
        {
            discovered_conventions.push(convention);
        }
    }

    // Phase 2b: Check signature consistency within conventions
    conventions::check_signature_consistency(&mut discovered_conventions, root, &audit_config);

    // Phase 3: Check all conventions
    let check_results = checks::check_conventions(&discovered_conventions);
    timing.push_ok("discovery_fingerprinting", discovery_started.elapsed());

    // Phase 4: Build findings
    let mut all_findings = findings::build_findings(&check_results);
    let detectors_started = std::time::Instant::now();

    // Phase 4b: Structural complexity analysis (god files, high item counts)
    let structural_findings = time_audit_detector(
        &mut timing,
        "detector.structural",
        plan.run_structural(),
        || structural::analyze_snapshot(root, &source_snapshot),
        Vec::new,
    );
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
    let convention_methods =
        build_convention_method_set(&discovered_conventions, &all_fingerprints);
    let detector_context = DetectorRunContext {
        root,
        audit_config: &audit_config,
        all_fingerprints: &all_fingerprints,
    };

    let duplication_findings = time_audit_detector(
        &mut timing,
        "detector.duplication.exact",
        plan.run_duplication(),
        || duplication::detect_duplicates(&all_fingerprints, &convention_methods),
        Vec::new,
    );
    let duplicate_groups = time_audit_detector(
        &mut timing,
        "detector.duplication.groups",
        plan.run_duplication(),
        || duplication::detect_duplicate_groups(&all_fingerprints),
        Vec::new,
    );
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
    let intra_dup_findings = time_audit_detector(
        &mut timing,
        "detector.duplication.intra_method",
        plan.run_duplication(),
        || duplication::detect_intra_method_duplicates(&all_fingerprints),
        Vec::new,
    );
    if !intra_dup_findings.is_empty() {
        log_status!(
            "audit",
            "Intra-method duplication: {} finding(s) (duplicated blocks within methods)",
            intra_dup_findings.len()
        );
        all_findings.extend(intra_dup_findings);
    }

    // Phase 4d: Near-duplicate detection (structural similarity)
    let near_dup_findings = time_audit_detector(
        &mut timing,
        "detector.duplication.near_duplicate",
        plan.run_duplication(),
        || duplication::detect_near_duplicates(&all_fingerprints),
        Vec::new,
    );
    if !near_dup_findings.is_empty() {
        log_status!(
            "audit",
            "Near-duplicates: {} finding(s) (structural matches with different identifiers)",
            near_dup_findings.len()
        );
        all_findings.extend(near_dup_findings);
    }

    // Phase 4d2: Parallel implementation detection (similar call patterns across files)
    let parallel_findings = time_audit_detector(
        &mut timing,
        "detector.duplication.parallel_implementation",
        plan.run_duplication(),
        || {
            duplication::detect_parallel_implementations(
                &all_fingerprints,
                &convention_methods,
                &audit_config.duplication_detector,
            )
        },
        Vec::new,
    );
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
    let ref_fingerprints = if plan.run_dead_code() {
        fingerprint_reference_paths(reference_paths)
    } else {
        Vec::new()
    };
    let component_ref_fingerprints = if plan.run_dead_code() {
        fingerprint_component_reference_files(root)
    } else {
        Vec::new()
    };
    let ref_fp_refs: Vec<&fingerprint::FileFingerprint> = ref_fingerprints
        .iter()
        .chain(component_ref_fingerprints.iter())
        .collect();
    let dead_code_findings = time_audit_detector(
        &mut timing,
        "detector.dead_code",
        plan.run_dead_code(),
        || dead_code::analyze_dead_code_with_config(&all_fingerprints, &ref_fp_refs, &audit_config),
        Vec::new,
    );
    if !dead_code_findings.is_empty() {
        log_status!(
            "audit",
            "Dead code: {} finding(s) (unused params, unreferenced exports, orphaned internals)",
            dead_code_findings.len()
        );
        all_findings.extend(dead_code_findings);
    }

    // Phase 4f: Comment hygiene detection (TODO/FIXME/HACK + stale phrasing)
    let comment_findings = time_audit_detector(
        &mut timing,
        "detector.comment_hygiene",
        plan.run_comment_hygiene(),
        || comment_hygiene::run(&all_fingerprints),
        Vec::new,
    );
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
    if plan.run_test_coverage() {
        let detector_started = std::time::Instant::now();
        if let Ok(comp) = component::load(component_id) {
            if let Some(extensions) = &comp.extensions {
                for ext_id in extensions.keys() {
                    if let Ok(ext_manifest) = crate::core::extension::load_extension(ext_id) {
                        if let Some(test_mapping) = ext_manifest.test_mapping() {
                            let coverage_findings =
                                test_coverage::run(root, &all_fingerprints, test_mapping);
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
        timing.push_ok("detector.test_coverage", detector_started.elapsed());
    } else {
        timing.push_skipped("detector.test_coverage");
    }

    // Phase 4h: Architecture/layer ownership rule checks (optional config)
    let layer_findings = time_audit_detector(
        &mut timing,
        "detector.layer_ownership",
        plan.run_layer_ownership(),
        || run_layer_ownership(root),
        Vec::new,
    );
    if !layer_findings.is_empty() {
        log_status!(
            "audit",
            "Layer ownership: {} finding(s) (architecture ownership violations)",
            layer_findings.len()
        );
        all_findings.extend(layer_findings);
    }

    run_descriptor_detectors(
        plan,
        &mut timing,
        &mut all_findings,
        &detector_context,
        &["test_topology", "test_wiring"],
    );

    // Phase 4j: Documentation drift detection (broken/stale references in markdown)
    let doc_findings = time_audit_detector(
        &mut timing,
        "detector.docs",
        plan.run_docs(),
        || detect_doc_drift(root, component_id),
        Vec::new,
    );
    if !doc_findings.is_empty() {
        log_status!(
            "audit",
            "Docs: {} finding(s) (broken references, stale paths)",
            doc_findings.len()
        );
        all_findings.extend(doc_findings);
    }

    // Phase 4l: Compiler warnings (dead code, unused imports, unused variables)
    // Runs extension-owned warning scripts and maps their output to findings.
    let compiler_findings = time_audit_detector(
        &mut timing,
        "detector.compiler_warnings",
        plan.run_compiler_warnings(),
        || compiler_warnings::run(root),
        Vec::new,
    );
    if !compiler_findings.is_empty() {
        log_status!(
            "audit",
            "Compiler warnings: {} finding(s) (dead code, unused imports, unused variables)",
            compiler_findings.len()
        );
        all_findings.extend(compiler_findings);
    }

    // Phase 4m: Wrapper-to-implementation inference
    // Detects wrapper files missing explicit declarations of what they wrap.
    // Uses configurable call pattern tracing to infer the implementation target.
    let wrapper_findings = time_audit_detector(
        &mut timing,
        "detector.wrapper_inference",
        plan.run_wrapper_inference(),
        || wrapper_inference::run(&all_fingerprints, root),
        Vec::new,
    );
    if !wrapper_findings.is_empty() {
        log_status!(
            "audit",
            "Wrapper inference: {} finding(s) (missing wrapper declarations)",
            wrapper_findings.len()
        );
        all_findings.extend(wrapper_findings);
    }

    run_descriptor_detectors(
        plan,
        &mut timing,
        &mut all_findings,
        &detector_context,
        &["shadow_modules"],
    );

    // Phase 4o: Repeated struct field pattern detection.
    let field_pattern_findings = time_audit_detector(
        &mut timing,
        "detector.field_patterns",
        plan.run_field_patterns(),
        || field_patterns::run(root),
        Vec::new,
    );
    if !field_pattern_findings.is_empty() {
        log_status!(
            "audit",
            "Field patterns: {} finding(s) (repeated struct fields)",
            field_pattern_findings.len()
        );
        all_findings.extend(field_pattern_findings);
    }

    run_descriptor_detectors(
        plan,
        &mut timing,
        &mut all_findings,
        &detector_context,
        &["facade_passthrough", "literal_shapes"],
    );

    // Phase 4r: Deprecation age detection
    let deprecation_findings = time_audit_detector(
        &mut timing,
        "detector.deprecation_age",
        plan.run_deprecation_age(),
        || deprecation_age::run(&all_fingerprints, root),
        Vec::new,
    );
    if !deprecation_findings.is_empty() {
        log_status!(
            "audit",
            "Deprecation age: {} finding(s) (stale @deprecated tags)",
            deprecation_findings.len()
        );
        all_findings.extend(deprecation_findings);
    }

    // Phase 4q: Dead guard detection — flag function_exists/class_exists/defined
    // guards on symbols guaranteed to exist given plugin requirements, composer
    // dependencies, and bootstrap requires.
    let dead_guard_findings = time_audit_detector(
        &mut timing,
        "detector.dead_guard",
        plan.run_dead_guard(),
        || dead_guard::run(&all_fingerprints, root, &audit_config),
        Vec::new,
    );
    if !dead_guard_findings.is_empty() {
        log_status!(
            "audit",
            "Dead guards: {} finding(s) (guards on guaranteed-available symbols)",
            dead_guard_findings.len()
        );
        all_findings.extend(dead_guard_findings);
    }

    // Phase 4t: Extension-owned requested detector rule packs.
    let requested_findings = time_audit_detector(
        &mut timing,
        "detector.requested_detectors",
        plan.run_requested_detectors(),
        || requested_detectors::run(&all_fingerprints, &audit_config),
        Vec::new,
    );
    if !requested_findings.is_empty() {
        log_status!(
            "audit",
            "Requested detectors: {} finding(s) (extension rule packs)",
            requested_findings.len()
        );
        all_findings.extend(requested_findings);
    }

    // Phase 4t1: Component-owned config-key write/accessor/read correlation.
    let config_key_findings = time_audit_detector(
        &mut timing,
        "detector.config_key_usage",
        plan.run_config_key_usage(),
        || config_key_usage::run(&all_fingerprints, &audit_config.config_key_usage.rules),
        Vec::new,
    );
    if !config_key_findings.is_empty() {
        log_status!(
            "audit",
            "Config key usage: {} finding(s) (write/accessor evidence without production reads)",
            config_key_findings.len()
        );
        all_findings.extend(config_key_findings);
    }

    // Phase 4t1b: Generic command-output capture hygiene.
    let output_capture_findings = time_audit_detector(
        &mut timing,
        "detector.output_capture",
        plan.run_output_capture(),
        || unbounded_output_capture::run(&all_fingerprints),
        Vec::new,
    );
    if !output_capture_findings.is_empty() {
        log_status!(
            "audit",
            "Output capture: {} finding(s) (unbounded stdout/stderr capture)",
            output_capture_findings.len()
        );
        all_findings.extend(output_capture_findings);
    }

    // Phase 4t2: Configured core-boundary ecosystem leak detection.
    let core_boundary_findings = time_audit_detector(
        &mut timing,
        "detector.core_boundary_leaks",
        plan.run_core_boundary_leaks(),
        || core_boundary_leak::run(&all_fingerprints, &audit_config.core_boundary_leaks),
        Vec::new,
    );
    if !core_boundary_findings.is_empty() {
        log_status!(
            "audit",
            "Core boundary leaks: {} finding(s) (configured ecosystem terms in core source)",
            core_boundary_findings.len()
        );
        all_findings.extend(core_boundary_findings);
    }

    // Phase 4t2b: Generic component-owned source policy checks.
    let source_policy_findings = time_audit_detector(
        &mut timing,
        "detector.source_policy",
        plan.run_source_policy(),
        || source_policy::run(&all_fingerprints, &audit_config.source_policies),
        Vec::new,
    );
    if !source_policy_findings.is_empty() {
        log_status!(
            "audit",
            "Source policy: {} finding(s) (configured source boundary rules)",
            source_policy_findings.len()
        );
        all_findings.extend(source_policy_findings);
    }

    // Phase 4t3: Configured mutating handler/resource access detection.
    let mutating_access_findings = time_audit_detector(
        &mut timing,
        "detector.mutating_resource_access",
        plan.run_mutating_resource_access(),
        || mutating_resource_access::run(&all_fingerprints, &audit_config.mutating_resource_access),
        Vec::new,
    );
    if !mutating_access_findings.is_empty() {
        log_status!(
            "audit",
            "Mutating resource access: {} finding(s) (resource mutations without configured access checks)",
            mutating_access_findings.len()
        );
        all_findings.extend(mutating_access_findings);
    }

    // Phase 4t4: Configured redirect-destination dominance checks.
    let redirect_validation_findings = time_audit_detector(
        &mut timing,
        "detector.redirect_validation",
        plan.run_redirect_validation(),
        || redirect_validation::run(&all_fingerprints, &audit_config.redirect_validation),
        Vec::new,
    );
    if !redirect_validation_findings.is_empty() {
        log_status!(
            "audit",
            "Redirect validation: {} finding(s) (request-derived redirects without dominating validation)",
            redirect_validation_findings.len()
        );
        all_findings.extend(redirect_validation_findings);
    }

    // Phase 4v: Process-global environment mutation guard consistency in tests.
    let env_guard_findings = time_audit_detector(
        &mut timing,
        "detector.global_env_guard",
        plan.run_global_env_guard(),
        || global_env_guard::run(&all_fingerprints),
        Vec::new,
    );
    if !env_guard_findings.is_empty() {
        log_status!(
            "audit",
            "Global env guards: {} finding(s) (test env mutation without shared guard)",
            env_guard_findings.len()
        );
        all_findings.extend(env_guard_findings);
    }

    run_descriptor_detectors(
        plan,
        &mut timing,
        &mut all_findings,
        &detector_context,
        &["shared_scaffolding"],
    );

    // Phase 4w: Parallel runner setup detection — command-family files that
    // assemble the same generic execution contract independently.
    let parallel_runner_findings = time_audit_detector(
        &mut timing,
        "detector.parallel_runner_setup",
        plan.run_parallel_runner_setup(),
        || parallel_runner_setup::run(&all_fingerprints),
        Vec::new,
    );
    if !parallel_runner_findings.is_empty() {
        log_status!(
            "audit",
            "Parallel runner setup: {} finding(s) (duplicated execution contract setup)",
            parallel_runner_findings.len()
        );
        all_findings.extend(parallel_runner_findings);
    }

    // Phase 4w1: Remote execution preflight detection — remote dispatch sites
    // that do not prove path/artifact parity before remote execution.
    let remote_execution_findings = time_audit_detector(
        &mut timing,
        "detector.remote_execution_preflight",
        plan.run_remote_execution_preflight(),
        || {
            remote_execution_preflight::run(
                &all_fingerprints,
                &audit_config.remote_execution_safety,
            )
        },
        Vec::new,
    );
    if !remote_execution_findings.is_empty() {
        log_status!(
            "audit",
            "Remote execution preflight: {} finding(s) (remote path/artifact parity gaps)",
            remote_execution_findings.len()
        );
        all_findings.extend(remote_execution_findings);
    }

    // Phase 4w: Repeated enum-dispatch contract detection.
    let enum_dispatch_findings = time_audit_detector(
        &mut timing,
        "detector.enum_dispatch_contracts",
        plan.run_enum_dispatch_contracts(),
        || enum_dispatch_contracts::run(root),
        Vec::new,
    );
    if !enum_dispatch_findings.is_empty() {
        log_status!(
            "audit",
            "Enum dispatch contracts: {} finding(s) (repeated exhaustive enum matches)",
            enum_dispatch_findings.len()
        );
        all_findings.extend(enum_dispatch_findings);
    }

    run_descriptor_detectors(
        plan,
        &mut timing,
        &mut all_findings,
        &detector_context,
        &["aggregate_construction"],
    );

    // Phase 4y: Public metadata routes returning raw registry/config getters
    // while a permission-aware resolver/helper exists in the same area.
    let public_registry_findings = time_audit_detector(
        &mut timing,
        "detector.public_registry_exposure",
        plan.run_public_registry_exposure(),
        || public_registry_exposure::run(&all_fingerprints, &audit_config.public_registry_exposure),
        Vec::new,
    );
    if !public_registry_findings.is_empty() {
        log_status!(
            "audit",
            "Public registry exposure: {} finding(s) (public metadata routes bypassing resolvers)",
            public_registry_findings.len()
        );
        all_findings.extend(public_registry_findings);
    }

    let artifact_portability_report = time_audit_detector(
        &mut timing,
        "detector.artifact_portability",
        plan.run_artifact_portability(),
        || {
            if audit_config.artifact_portability.is_empty() {
                artifact_portability::run_report(component_id)
            } else {
                artifact_portability::run_report_with_config(
                    component_id,
                    &audit_config.artifact_portability,
                )
            }
        },
        Default::default,
    );
    if plan.run_artifact_portability() {
        log_status!(
            "audit",
            "Artifact portability: scanned {} recent run(s), {} artifact row(s), {} metadata string field(s) (window: {})",
            artifact_portability_report.runs_scanned,
            artifact_portability_report.artifacts_scanned,
            artifact_portability_report.metadata_fields_scanned,
            artifact_portability_report.run_window
        );
    }
    let artifact_portability_findings = artifact_portability_report.findings;
    if !artifact_portability_findings.is_empty() {
        log_status!(
            "audit",
            "Artifact portability: {} finding(s) (non-portable artifact evidence paths)",
            artifact_portability_findings.len()
        );
        all_findings.extend(artifact_portability_findings);
    }

    // Phase 4z: Declared command status scenario fixtures.
    let command_status_findings = time_audit_detector(
        &mut timing,
        "detector.command_status_contracts",
        plan.run_command_status_contracts(),
        || command_status_contracts::run(root, &audit_config.command_status_contracts),
        Vec::new,
    );
    if !command_status_findings.is_empty() {
        log_status!(
            "audit",
            "Command status contracts: {} finding(s) (inconsistent no-op/dry-run status fields)",
            command_status_findings.len()
        );
        all_findings.extend(command_status_findings);
    }
    timing.push_ok("detectors", detectors_started.elapsed());

    // Phase 4p: Impact-scoped filtering — when auditing changed files only,
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
    let report_started = std::time::Instant::now();
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
    timing.push_ok("report", report_started.elapsed());

    drop(all_fingerprints);
    let analysis = AuditAnalysisContext {
        fingerprints: discovery
            .groups
            .into_iter()
            .flat_map(|(_, _, fingerprints)| fingerprints)
            .collect(),
    };

    Ok(AuditWithAnalysis {
        result: CodeAuditResult {
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
        },
        analysis,
        timing,
    })
}

fn audit_root_only(
    component_id: &str,
    source_path: &str,
    root: &Path,
    plan: &AuditExecutionPlan,
    extension_overrides: &[String],
    timing: &mut AuditTiming,
) -> CodeAuditResult {
    let audit_config = audit_config_for(component_id, root, extension_overrides);
    let mut findings = Vec::new();
    let detector_context = DetectorRunContext {
        root,
        audit_config: &audit_config,
        all_fingerprints: &[],
    };

    let structural_findings = time_audit_detector(
        timing,
        "detector.structural",
        plan.run_structural(),
        || structural::analyze_structure(root),
        Vec::new,
    );
    if !structural_findings.is_empty() {
        log_status!(
            "audit",
            "Structural: {} finding(s) (god files, high item counts)",
            structural_findings.len()
        );
        findings.extend(structural_findings);
    }

    let layer_findings = time_audit_detector(
        timing,
        "detector.layer_ownership",
        plan.run_layer_ownership(),
        || run_layer_ownership(root),
        Vec::new,
    );
    if !layer_findings.is_empty() {
        log_status!(
            "audit",
            "Layer ownership: {} finding(s) (architecture ownership violations)",
            layer_findings.len()
        );
        findings.extend(layer_findings);
    }

    run_descriptor_detectors(
        plan,
        timing,
        &mut findings,
        &detector_context,
        &["test_topology", "test_wiring"],
    );

    let doc_findings = time_audit_detector(
        timing,
        "detector.docs",
        plan.run_docs(),
        || detect_doc_drift(root, component_id),
        Vec::new,
    );
    if !doc_findings.is_empty() {
        log_status!(
            "audit",
            "Docs: {} finding(s) (broken references, stale paths)",
            doc_findings.len()
        );
        findings.extend(doc_findings);
    }

    let compiler_findings = time_audit_detector(
        timing,
        "detector.compiler_warnings",
        plan.run_compiler_warnings(),
        || compiler_warnings::run(root),
        Vec::new,
    );
    if !compiler_findings.is_empty() {
        log_status!(
            "audit",
            "Compiler warnings: {} finding(s) (dead code, unused imports, unused variables)",
            compiler_findings.len()
        );
        findings.extend(compiler_findings);
    }

    let field_pattern_findings = time_audit_detector(
        timing,
        "detector.field_patterns",
        plan.run_field_patterns(),
        || field_patterns::run(root),
        Vec::new,
    );
    if !field_pattern_findings.is_empty() {
        log_status!(
            "audit",
            "Field patterns: {} finding(s) (repeated struct fields)",
            field_pattern_findings.len()
        );
        findings.extend(field_pattern_findings);
    }

    let artifact_portability_report = time_audit_detector(
        timing,
        "detector.artifact_portability",
        plan.run_artifact_portability(),
        || {
            if audit_config.artifact_portability.is_empty() {
                artifact_portability::run_report(component_id)
            } else {
                artifact_portability::run_report_with_config(
                    component_id,
                    &audit_config.artifact_portability,
                )
            }
        },
        Default::default,
    );
    if plan.run_artifact_portability() {
        log_status!(
            "audit",
            "Artifact portability: scanned {} recent run(s), {} artifact row(s), {} metadata string field(s) (window: {})",
            artifact_portability_report.runs_scanned,
            artifact_portability_report.artifacts_scanned,
            artifact_portability_report.metadata_fields_scanned,
            artifact_portability_report.run_window
        );
    }
    let artifact_portability_findings = artifact_portability_report.findings;
    if !artifact_portability_findings.is_empty() {
        log_status!(
            "audit",
            "Artifact portability: {} finding(s) (non-portable artifact evidence paths)",
            artifact_portability_findings.len()
        );
        findings.extend(artifact_portability_findings);
    }

    let outliers_found = findings.len();
    log_status!(
        "audit",
        "Complete: root-only filtered run, {} finding(s)",
        outliers_found
    );

    CodeAuditResult {
        component_id: component_id.to_string(),
        source_path: source_path.to_string(),
        summary: AuditSummary {
            files_scanned: 0,
            conventions_detected: 0,
            outliers_found,
            alignment_score: None,
            files_skipped: 0,
            warnings: vec![],
        },
        conventions: vec![],
        directory_conventions: vec![],
        findings,
        duplicate_groups: vec![],
    }
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
        crate::core::component::scope::resolve_component_scope(
            &comp,
            crate::core::component::scope::ScopeCommand::Audit,
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
        for parent_methods in method_by_parent.values() {
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

        // Slice 2 of #1492: snapshot once, fingerprint from in-memory content
        // instead of re-reading each file inside `fingerprint_file`.
        let snapshot = walker::walk_source_files_snapshot(root);
        for (path, content) in snapshot.iter() {
            if let Some(fp) = fingerprint::fingerprint_content(path, root, content) {
                ref_fps.push(fp);
                total_files += 1;
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

/// Fingerprint this component's source files as dead-code references.
///
/// Dead-code findings are still emitted only for the owned convention
/// fingerprints. This reference-only pass keeps calls from singleton files,
/// index files, and module facades in the graph so exported functions are not
/// reported just because their consumers live outside a convention group.
fn fingerprint_component_reference_files(root: &Path) -> Vec<fingerprint::FileFingerprint> {
    let snapshot = walker::walk_all_source_files_snapshot(root);
    let mut fingerprints = Vec::new();

    for (path, content) in snapshot.iter() {
        if let Some(fp) = fingerprint::fingerprint_content(path, root, content) {
            fingerprints.push(fp);
        }
    }

    fingerprints
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
    fn only_vacuous_test_keeps_all_vacuous_detector_families_enabled() {
        let plan = AuditExecutionPlan::from_filters(&[AuditFinding::VacuousTest], &[]);

        assert!(
            plan.run_test_coverage(),
            "coverage detector also emits vacuous_test for mapped tests"
        );
        assert!(
            plan.detector_enabled("test_topology"),
            "test topology/test quality detector emits standalone vacuous_test findings"
        );
    }

    #[test]
    fn detector_timing_records_successful_span() {
        let mut timing = AuditTiming::default();
        let value = time_audit_detector(&mut timing, "detector.structural", true, || 42, || 0);

        assert_eq!(value, 42);
        assert_eq!(timing.spans.len(), 1);
        assert_eq!(timing.spans[0].id, "detector.structural");
        assert_eq!(timing.spans[0].status, "ok");
        assert!(timing.spans[0].duration_ms.is_some());
    }

    #[test]
    fn detector_timing_records_disabled_span_as_skipped() {
        let mut timing = AuditTiming::default();
        let value = time_audit_detector(
            &mut timing,
            "detector.duplication.exact",
            false,
            || 42,
            || 0,
        );

        assert_eq!(value, 0);
        assert_eq!(timing.spans.len(), 1);
        assert_eq!(timing.spans[0].id, "detector.duplication.exact");
        assert_eq!(timing.spans[0].status, "skipped");
        assert!(timing.spans[0].duration_ms.is_none());
    }

    #[test]
    fn audit_config_includes_explicit_extension_overrides() {
        crate::test_support::with_isolated_home(|home| {
            let mut manifest: crate::core::extension::ExtensionManifest =
                serde_json::from_value(serde_json::json!({
                    "name": "Fixture",
                    "version": "0.0.0",
                    "audit": {
                        "detector_rules": {
                            "convention_exception_globs": ["scripts/lint/fixer-helpers.fixture"]
                        }
                    }
                }))
                .expect("manifest");
            manifest.id = "fixture".to_string();
            crate::core::extension::save_manifest(&manifest).expect("save fixture extension");

            let config = audit_config_for(
                "component-without-extension",
                home.path(),
                &["fixture".to_string()],
            );

            assert!(
                config
                    .convention_exception_globs
                    .contains(&"scripts/lint/fixer-helpers.fixture".to_string()),
                "explicit --extension audit rules should feed audit detector config"
            );
        });
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

        let findings = detectors::layer_ownership::run(&dir);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].convention, "layer_ownership");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_code_reference_fingerprints_include_rust_singleton_and_index_files() {
        let dir =
            std::env::temp_dir().join(format!("homeboy_audit_index_refs_{}", std::process::id()));
        let src = dir.join("src");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&src).unwrap();

        fs::write(src.join("foo.rs"), "pub fn helper() -> bool { true }\n").unwrap();
        fs::write(
            src.join("main.rs"),
            "mod foo;\nfn main() { let _ = foo::helper(); }\n",
        )
        .unwrap();

        let regular_snapshot = walker::walk_source_files_snapshot(&dir);
        let owned_fingerprints: Vec<_> = regular_snapshot
            .iter()
            .filter_map(|(path, content)| fingerprint::fingerprint_content(path, &dir, content))
            .collect();
        let component_ref_fingerprints = fingerprint_component_reference_files(&dir);

        let owned_refs: Vec<_> = owned_fingerprints.iter().collect();
        let component_refs: Vec<_> = component_ref_fingerprints.iter().collect();
        let findings = dead_code::analyze_dead_code_with_config(
            &owned_refs,
            &component_refs,
            &AuditConfig::default(),
        );
        let unreferenced: Vec<_> = findings
            .iter()
            .filter(|finding| finding.kind == AuditFinding::UnreferencedExport)
            .collect();

        assert!(
            unreferenced.is_empty(),
            "singleton and index files should contribute reference-only calls for dead-code analysis, got: {:?}",
            unreferenced
                .iter()
                .map(|finding| &finding.description)
                .collect::<Vec<_>>()
        );

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
