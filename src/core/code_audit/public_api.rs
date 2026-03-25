//! public_api — extracted from mod.rs.

use std::path::Path;
use self::layer_ownership::run as run_layer_ownership;
use crate::{component, is_zero, Result};
use crate::core::code_audit::AuditSummary;
use crate::core::code_audit::CodeAuditResult;
use crate::core::code_audit::build_convention_method_set;
use crate::core::code_audit::ConventionReport;
use crate::core::code_audit::fingerprint_reference_paths;


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
pub(crate) fn read_reference_paths_from_env() -> Vec<String> {
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
    audit_internal(
        component_id,
        source_path,
        Some(file_filter),
        git_ref,
        &ref_paths,
    )
}

/// Internal audit implementation supporting optional file scoping and impact tracing.
///
/// `reference_paths` are external codebases whose fingerprints are included in
/// cross-reference analysis (dead code) but excluded from convention discovery,
/// duplication detection, and structural analysis.
pub(crate) fn audit_internal(
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
    let convention_methods =
        build_convention_method_set(&discovered_conventions, &all_fingerprints);

    let duplication_findings =
        duplication::detect_duplicates(&all_fingerprints, &convention_methods);
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

    // Phase 4m: Wrapper-to-implementation inference
    // Detects wrapper files missing explicit declarations of what they wrap.
    // Uses configurable call pattern tracing to infer the implementation target.
    let wrapper_findings = wrapper_inference::analyze_wrappers(&all_fingerprints, root);
    if !wrapper_findings.is_empty() {
        log_status!(
            "audit",
            "Wrapper inference: {} finding(s) (missing wrapper declarations)",
            wrapper_findings.len()
        );
        all_findings.extend(wrapper_findings);
    }

    // Phase 4n: Shadow module detection — directories that are near-copies.
    let shadow_findings = shadow_modules::run(&all_fingerprints);
    if !shadow_findings.is_empty() {
        log_status!(
            "audit",
            "Shadow modules: {} finding(s) (duplicate directory structures)",
            shadow_findings.len()
        );
        all_findings.extend(shadow_findings);
    }

    // Phase 4o: Repeated struct field pattern detection.
    let field_pattern_findings = field_patterns::run(root);
    if !field_pattern_findings.is_empty() {
        log_status!(
            "audit",
            "Field patterns: {} finding(s) (repeated struct fields)",
            field_pattern_findings.len()
        );
        all_findings.extend(field_pattern_findings);
    }

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

    #[test]
    #[ignore = "Requires PHP extension with fingerprint script installed"]
    pub(crate) fn audit_directory_with_convention() {
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
