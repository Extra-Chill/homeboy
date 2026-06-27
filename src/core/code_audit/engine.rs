//! Core audit execution engine — the full discovery + detector pipeline and the
//! root-only fast path.
//!
//! Mechanically split out of `mod.rs`; behavior and the surrounding public API
//! are preserved by the re-exports in the module root.
//!
//! Detector dispatch is table-driven: the descriptor table in `execution_plan`
//! declares every detector and `descriptor_runtime::run_descriptor_detectors`
//! executes it. Only three families are still sequenced by hand below because
//! they have a non-uniform shape — the convention pipeline, the multi-pass
//! `duplication` family (five timing spans plus the `duplicate_groups` side
//! output), and `artifact_portability` (logs scan statistics even when empty).

use std::collections::HashSet;
use std::path::Path;

use super::descriptor_runtime::{run_descriptor_detectors, DetectorRunContext};
use super::detectors::{artifact_portability, field_patterns};
use super::entry::audit_config_for;
use super::execution_plan::AuditExecutionPlan;
use super::findings;
use super::reference::build_convention_method_set;
use super::types::{
    time_audit_detector, AuditAnalysisContext, AuditSummary, AuditTiming, AuditWithAnalysis,
    CodeAuditResult, ConventionReport, ScopedAuditExecution,
};
use super::{checks, conventions, discovery, duplication, fingerprint, impact, structural, walker};
use crate::core::component::AuditConfig;
use crate::core::Result;

/// Detectors run on the root-only fast path (no discovery/fingerprinting). The
/// full path drives every data-driven detector via `ids = None`; this subset
/// keeps the fast path's timing spans and findings identical to before.
const ROOT_ONLY_DETECTOR_IDS: &[&str] = &[
    "structural",
    "layer_ownership",
    "test_topology",
    "test_wiring",
    "docs",
    "compiler_warnings",
    "field_patterns",
    "command_status_contracts",
    "thin_command_adapter",
];

/// Internal audit implementation supporting optional file scoping and impact tracing.
///
/// `reference_paths` are external codebases whose fingerprints are included in
/// cross-reference analysis (dead code) but excluded from convention discovery,
/// duplication detection, and structural analysis.
pub(super) fn audit_internal(
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
    let scoped_execution = ScopedAuditExecution::new(file_filter, git_ref);

    if scoped_execution.is_scoped() {
        log_status!(
            "audit",
            "Scoped audit: {} changed file(s), impact tracing {}",
            scoped_execution.changed_file_count(),
            if scoped_execution.impact_tracing_enabled() {
                "enabled"
            } else {
                "disabled"
            }
        );
        log_status!("audit", "Scanning {} for conventions...", source_path);
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
            scoped_execution.file_filter,
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
    let source_snapshot_started = std::time::Instant::now();
    let source_snapshot = build_shared_source_snapshot(root, &audit_config);
    timing.push_ok("source_snapshot", source_snapshot_started.elapsed());
    if scoped_execution.is_scoped() {
        log_status!(
            "audit",
            "Scoped audit snapshot: {} full-codebase source file(s) captured for convention discovery",
            source_snapshot.len()
        );
    }
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

    // Phase 4b2: In scoped (--changed-since) mode, compute the touched-file scope
    // ONCE up front so per-file detectors only walk the fingerprints they could
    // possibly emit reportable findings for. Whole-tree detector runs are the
    // root cause of the changed-scope timeout: every per-file content scanner
    // (comment hygiene, deprecation age, dead guards, source/boundary policy,
    // env guards, redirect/resource/config-key checks) re-scanned the entire
    // repository even though out-of-scope findings are discarded by the Phase 4p
    // scope filter anyway. These detectors emit findings keyed strictly to the
    // file they inspect, so restricting their input to the scoped fingerprint
    // subset is behavior-preserving while avoiding O(repo) work per detector.
    //
    // Cross-file detectors (duplication, dead code) still receive the full
    // corpus below because their correctness for an in-scope file depends on
    // evidence from out-of-scope files (matching bodies, external references).
    let scoped_fingerprints: Option<(HashSet<String>, Vec<&fingerprint::FileFingerprint>)> =
        scoped_execution.file_filter.map(|filter| {
            let scope_started = std::time::Instant::now();
            let scope_files: HashSet<String> = if let Some(ref_str) = git_ref {
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
                scoped_execution.changed_files.clone()
            };
            let subset: Vec<&fingerprint::FileFingerprint> = all_fingerprints
                .iter()
                .copied()
                .filter(|fp| {
                    scope_files
                        .iter()
                        .any(|scope| fp.relative_path.contains(scope.as_str()))
                })
                .collect();
            log_status!(
                "audit",
                "Scoped detectors: {} of {} fingerprint(s) in touched scope ({} scoped file(s))",
                subset.len(),
                all_fingerprints.len(),
                scope_files.len()
            );
            timing.push_ok("scope.fingerprints", scope_started.elapsed());
            (scope_files, subset)
        });

    // Per-file detector input: the scoped subset when in changed-scope mode,
    // else the full corpus. Cross-file detectors keep using `all_fingerprints`.
    let per_file_fingerprints: &[&fingerprint::FileFingerprint] = scoped_fingerprints
        .as_ref()
        .map(|(_, subset)| subset.as_slice())
        .unwrap_or(all_fingerprints.as_slice());

    let detector_context = DetectorRunContext {
        root,
        component_id,
        audit_config: &audit_config,
        all_fingerprints: &all_fingerprints,
        per_file_fingerprints,
        source_snapshot: Some(&source_snapshot),
        reference_paths,
    };

    // The duplication family stays hand-sequenced: it runs five timing spans and
    // also produces `duplicate_groups`, a side output threaded into the report.
    let duplication_findings = time_audit_detector(
        &mut timing,
        "detector.duplication.exact",
        plan.detector_enabled("duplication"),
        || duplication::detect_duplicates(&all_fingerprints, &convention_methods),
        Vec::new,
    );
    let duplicate_groups = time_audit_detector(
        &mut timing,
        "detector.duplication.groups",
        plan.detector_enabled("duplication"),
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
        plan.detector_enabled("duplication"),
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
        plan.detector_enabled("duplication"),
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
        plan.detector_enabled("duplication"),
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

    // Every other detector — structural, dead code, comment hygiene, the policy
    // packs, the fingerprint/root families, etc. — is dispatched by the
    // descriptor table in one pass. Adding a detector is a descriptor row plus a
    // `run_generic_descriptor` arm, never a new block here.
    run_descriptor_detectors(plan, &mut timing, &mut all_findings, &detector_context, None);

    // `artifact_portability` stays hand-sequenced: it logs scan statistics (runs,
    // artifacts, metadata fields) even when it produces no findings.
    let artifact_portability_report = time_audit_detector(
        &mut timing,
        "detector.artifact_portability",
        plan.detector_enabled("artifact_portability"),
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
    if plan.detector_enabled("artifact_portability") {
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
    timing.push_ok("detectors", detectors_started.elapsed());

    // Phase 4p: Impact-scoped filtering — when auditing changed files only,
    // filter findings down to the touched scope (changed files + impact call
    // sites). The scope set and its impact expansion were already computed once
    // in Phase 4b2; reuse it here instead of re-running the O(repo) impact diff,
    // and to guarantee the per-file detector input and the finding filter agree
    // on the same scope.
    if let Some((scope_files, _)) = scoped_fingerprints.as_ref() {
        let scope_started = std::time::Instant::now();
        let before = all_findings.len();

        log_status!(
            "audit",
            "Scoped audit filter: {} changed file(s), {} total scoped file(s)",
            scoped_execution.changed_file_count(),
            scope_files.len()
        );

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
        timing.push_ok("scope.filter", scope_started.elapsed());
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

    // Release the scoped fingerprint subset (borrows from `all_fingerprints`)
    // before dropping the owning corpus. `per_file_fingerprints` is just a
    // borrowed view, so its borrow ends here via NLL.
    drop(scoped_fingerprints);
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

/// Build the shared audit source snapshot consumed by whole-tree detectors.
///
/// The snapshot is a single walk/read of the codebase whose extension set is a
/// superset of every snapshot-backed detector's inputs (structural source
/// extensions plus the field-pattern detector's resolved scan tokens). This
/// lets those detectors filter the shared snapshot instead of each re-walking
/// and re-reading the tree, while keeping their inputs — and therefore their
/// findings — identical.
fn build_shared_source_snapshot(
    root: &Path,
    audit_config: &AuditConfig,
) -> crate::core::engine::codebase_scan::CodebaseSnapshot {
    let mut additional: Vec<String> = structural::source_extensions()
        .iter()
        .map(|ext| (*ext).to_string())
        .collect();
    additional.extend(field_patterns::scan_token_extensions(
        &audit_config.detector_profile,
    ));
    additional.sort();
    additional.dedup();
    let additional_refs: Vec<&str> = additional.iter().map(|s| s.as_str()).collect();
    walker::walk_shared_audit_files_snapshot(root, &additional_refs)
}

fn audit_root_only(
    component_id: &str,
    source_path: &str,
    root: &Path,
    plan: &AuditExecutionPlan,
    extension_overrides: &[String],
    file_filter: Option<&[String]>,
    timing: &mut AuditTiming,
) -> CodeAuditResult {
    let audit_config = audit_config_for(component_id, root, extension_overrides);
    let mut findings = Vec::new();

    // Build the shared source snapshot once for the root-only path so the
    // whole-tree detectors below (structural, field patterns) consume a single
    // walk/read instead of each re-walking and re-reading the tree. Built only
    // when at least one snapshot-backed detector is enabled.
    let source_snapshot = if plan.detector_enabled("structural")
        || plan.detector_enabled("field_patterns")
    {
        let snapshot_started = std::time::Instant::now();
        let snapshot = build_shared_source_snapshot(root, &audit_config);
        timing.push_ok("source_snapshot", snapshot_started.elapsed());
        Some(snapshot)
    } else {
        None
    };

    let detector_context = DetectorRunContext {
        root,
        component_id,
        audit_config: &audit_config,
        all_fingerprints: &[],
        per_file_fingerprints: &[],
        source_snapshot: source_snapshot.as_ref(),
        reference_paths: &[],
    };

    run_descriptor_detectors(
        plan,
        timing,
        &mut findings,
        &detector_context,
        Some(ROOT_ONLY_DETECTOR_IDS),
    );

    // `artifact_portability` stays hand-sequenced (logs scan statistics even
    // when empty), mirroring the full path.
    let artifact_portability_report = time_audit_detector(
        timing,
        "detector.artifact_portability",
        plan.detector_enabled("artifact_portability"),
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
    if plan.detector_enabled("artifact_portability") {
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

    if let Some(filter) = file_filter {
        let scope_started = std::time::Instant::now();
        let before = findings.len();
        findings.retain(|finding| filter.iter().any(|scope| finding.file.contains(scope)));
        let filtered_out = before - findings.len();
        if filtered_out > 0 {
            log_status!(
                "audit",
                "Scoped: filtered {} root-only finding(s) from out-of-scope files ({} remaining)",
                filtered_out,
                findings.len()
            );
        }
        timing.push_ok("scope.filter", scope_started.elapsed());
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
