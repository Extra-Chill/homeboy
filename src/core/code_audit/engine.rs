//! Core audit execution engine — the full discovery + detector pipeline and the
//! root-only fast path.
//!
//! Mechanically split out of `mod.rs`; behavior and the surrounding public API
//! are preserved by the re-exports in the module root.

use std::collections::HashSet;
use std::path::Path;

use super::descriptor_runtime::{run_descriptor_detectors, DetectorRunContext};
use super::detectors::layer_ownership::run as run_layer_ownership;
use super::detectors::{
    artifact_portability, command_status_contracts, config_key_usage, dead_guard, deprecation_age,
    enum_dispatch_contracts, field_patterns, global_env_guard, mutating_resource_access,
    parallel_runner_setup, public_registry_exposure, redirect_validation,
    remote_execution_preflight, requested_detectors, source_policy, test_coverage,
    thin_command_adapter, unbounded_output_capture, wrapper_inference,
};
use super::doc_drift::detect_doc_drift;
use super::entry::audit_config_for;
use super::execution_plan::AuditExecutionPlan;
use super::findings;
use super::reference::{
    build_convention_method_set, fingerprint_component_reference_files, fingerprint_reference_paths,
};
use super::types::{
    time_audit_detector, AuditAnalysisContext, AuditSummary, AuditTiming, AuditWithAnalysis,
    CodeAuditResult, ConventionReport, ScopedAuditExecution,
};
use super::{
    checks, comment_hygiene, compiler_warnings, conventions, dead_code, discovery, duplication,
    fingerprint, impact, structural, walker,
};
use crate::core::component::{self, AuditConfig};
use crate::core::Result;

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
        || comment_hygiene::run(per_file_fingerprints, &audit_config.detector_profile),
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
        || field_patterns::run(&source_snapshot, &audit_config.detector_profile),
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
        || deprecation_age::run(&all_fingerprints, root, &audit_config.detector_profile),
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
        || dead_guard::run(per_file_fingerprints, root, &audit_config),
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
        || unbounded_output_capture::run(per_file_fingerprints),
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
        || {
            source_policy::run(
                per_file_fingerprints,
                &audit_config.core_boundary_leaks.to_source_policy_rules(),
            )
        },
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
        || source_policy::run(per_file_fingerprints, &audit_config.source_policies),
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
        || {
            mutating_resource_access::run(
                per_file_fingerprints,
                &audit_config.mutating_resource_access,
            )
        },
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
        || redirect_validation::run(per_file_fingerprints, &audit_config.redirect_validation),
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
        || enum_dispatch_contracts::run(&source_snapshot),
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

    // Phase 4za: Thin-command-adapter boundary checks. Flags command-layer
    // modules that accumulate orchestration/business logic instead of staying
    // thin adapters over core services.
    let thin_command_adapter_findings = time_audit_detector(
        &mut timing,
        "detector.thin_command_adapter",
        plan.run_thin_command_adapter(),
        || thin_command_adapter::run(root, &audit_config.thin_command_adapter),
        Vec::new,
    );
    if !thin_command_adapter_findings.is_empty() {
        log_status!(
            "audit",
            "Thin command adapters: {} finding(s) (command modules accumulating orchestration logic)",
            thin_command_adapter_findings.len()
        );
        all_findings.extend(thin_command_adapter_findings);
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
    let detector_context = DetectorRunContext {
        root,
        audit_config: &audit_config,
        all_fingerprints: &[],
    };

    // Build the shared source snapshot once for the root-only path so the
    // whole-tree detectors below (structural, field patterns) consume a single
    // walk/read instead of each re-walking and re-reading the tree. Built only
    // when at least one snapshot-backed detector is enabled.
    let source_snapshot = if plan.run_structural() || plan.run_field_patterns() {
        let snapshot_started = std::time::Instant::now();
        let snapshot = build_shared_source_snapshot(root, &audit_config);
        timing.push_ok("source_snapshot", snapshot_started.elapsed());
        Some(snapshot)
    } else {
        None
    };

    let structural_findings = time_audit_detector(
        timing,
        "detector.structural",
        plan.run_structural(),
        || match source_snapshot.as_ref() {
            Some(snapshot) => structural::analyze_snapshot(root, snapshot),
            None => structural::analyze_structure(root),
        },
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
        || match source_snapshot.as_ref() {
            Some(snapshot) => field_patterns::run(snapshot, &audit_config.detector_profile),
            None => Vec::new(),
        },
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

    let command_status_findings = time_audit_detector(
        timing,
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
        findings.extend(command_status_findings);
    }

    let thin_command_adapter_findings = time_audit_detector(
        timing,
        "detector.thin_command_adapter",
        plan.run_thin_command_adapter(),
        || thin_command_adapter::run(root, &audit_config.thin_command_adapter),
        Vec::new,
    );
    if !thin_command_adapter_findings.is_empty() {
        log_status!(
            "audit",
            "Thin command adapters: {} finding(s) (command modules accumulating orchestration logic)",
            thin_command_adapter_findings.len()
        );
        findings.extend(thin_command_adapter_findings);
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
