use super::detectors::layer_ownership::run as run_layer_ownership;
use super::detectors::{
    aggregate_construction, command_status_contracts, command_wrapper_bypass, config_key_usage,
    constant_bypass, dead_guard, deprecation_age, enum_dispatch_contracts, facade_passthrough,
    global_env_guard, mutating_resource_access, parallel_runner_setup, policy_flow,
    public_registry_exposure, redirect_validation, remote_execution_preflight,
    repeated_literal_shape, requested_detectors, shared_scaffolding, source_policy, test_coverage,
    test_topology, test_wiring, thin_command_adapter, unbounded_output_capture, wrapper_inference,
};
use super::doc_drift::detect_doc_drift;
use super::reference::DeadCodeReferenceAnalysis;
use super::{
    comment_hygiene, compiler_warnings, dead_code, fingerprint, shadow_modules, structural,
    time_audit_detector_isolated, AuditExecutionPlan, AuditTiming, AuditTimingSpan,
    DetectorDescriptor, DetectorRuntime, Finding, FingerprintDetectorRunner, GenericDetectorRunner,
    RootDetectorRunner,
};
use homeboy_audit_contract::AuditConfig;
use homeboy_engine_primitives::codebase_scan::CodebaseSnapshot;
use std::path::Path;

/// Inputs shared by every data-driven detector. The descriptor table dispatches
/// through [`run_descriptor_detectors`]; each runner reads only the fields it
/// needs from this context, so adding a detector means one descriptor row plus
/// one match arm — never a new hand-wired block in `engine.rs`.
pub(super) struct DetectorRunContext<'a> {
    pub(super) root: &'a Path,
    pub(super) component_id: &'a str,
    pub(super) audit_config: &'a AuditConfig,
    /// Full fingerprint corpus. Cross-file detectors (dead code, duplication
    /// inputs) need the whole tree even when the audit is scoped.
    pub(super) all_fingerprints: &'a [&'a fingerprint::FileFingerprint],
    /// Per-file detector input: the scoped subset under `--changed-since`, else
    /// the full corpus. Detectors keyed strictly to the file they inspect read
    /// this so scoped runs avoid O(repo) work.
    pub(super) per_file_fingerprints: &'a [&'a fingerprint::FileFingerprint],
    /// Source-policy corpus: every extension-claimed source file, index files
    /// (`mod.rs`/`lib.rs`/`main.rs`) included and single-file directories kept,
    /// narrowed to the changed scope under `--changed-since` (#10558).
    ///
    /// Distinct from `per_file_fingerprints` because that view is derived from
    /// the CONVENTION corpus, whose index-file exclusion and `len() < 2` group
    /// drop exist for sibling detection and are meaningless for a term scan.
    /// Detectors that scan file text for configured patterns read this.
    pub(super) policy_fingerprints: &'a [&'a fingerprint::FileFingerprint],
    /// Shared source snapshot for whole-tree detectors. `None` only on the
    /// root-only fast path when no snapshot-backed detector is enabled.
    pub(super) source_snapshot: Option<&'a CodebaseSnapshot>,
    pub(super) dead_code_references: Option<&'a DeadCodeReferenceAnalysis>,
    /// The Rust test-quality walk is shared by topology and coverage so a full
    /// audit classifies each test file once before descriptor fan-out.
    pub(super) test_quality_findings: Option<&'a [Finding]>,
}

fn run_fingerprint_descriptor(
    runner: FingerprintDetectorRunner,
    context: &DetectorRunContext<'_>,
) -> Vec<Finding> {
    match runner {
        FingerprintDetectorRunner::ShadowModules => shadow_modules::run(context.all_fingerprints),
        FingerprintDetectorRunner::FacadePassthrough => {
            facade_passthrough::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::LiteralShapes => repeated_literal_shape::run(
            context.all_fingerprints,
            &context
                .audit_config
                .detector_profile
                .repeated_literal_shape_extensions,
        ),
        FingerprintDetectorRunner::ConstantBypass => constant_bypass::run(context.all_fingerprints),
        FingerprintDetectorRunner::CommandWrapperBypass => {
            command_wrapper_bypass::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::SharedScaffolding => {
            shared_scaffolding::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::AggregateConstruction => {
            aggregate_construction::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::PolicyFlow => {
            policy_flow::run(context.all_fingerprints, &context.audit_config.policy_flow)
        }
    }
}

fn run_root_descriptor(
    runner: RootDetectorRunner,
    context: &DetectorRunContext<'_>,
) -> Vec<Finding> {
    match runner {
        RootDetectorRunner::TestTopology => context.test_quality_findings.map_or_else(
            || test_topology::run(context.root),
            test_topology::run_from_shared,
        ),
        RootDetectorRunner::TestWiring => test_wiring::run(context.root, context.audit_config),
    }
}

/// Dispatch a single-closure detector. Each arm is the one invocation that used
/// to live in a hand-wired `time_audit_detector` block in `engine.rs`; the
/// descriptor table now supplies enable state, timing id, and logging.
fn run_generic_descriptor(
    runner: GenericDetectorRunner,
    context: &DetectorRunContext<'_>,
) -> Vec<Finding> {
    let config = context.audit_config;
    match runner {
        GenericDetectorRunner::Structural => match context.source_snapshot {
            Some(snapshot) => structural::analyze_snapshot(context.root, snapshot),
            None => structural::analyze_structure(context.root),
        },
        GenericDetectorRunner::DeadCode => run_dead_code(context),
        GenericDetectorRunner::CommentHygiene => {
            comment_hygiene::run(context.per_file_fingerprints, &config.detector_profile)
        }
        GenericDetectorRunner::TestCoverage => run_test_coverage(context),
        GenericDetectorRunner::LayerOwnership => run_layer_ownership(context.root),
        GenericDetectorRunner::Docs => detect_doc_drift(context.root, context.component_id),
        GenericDetectorRunner::CompilerWarnings => compiler_warnings::run(context.root),
        GenericDetectorRunner::WrapperInference => {
            wrapper_inference::run(context.all_fingerprints, context.root)
        }
        GenericDetectorRunner::DeprecationAge => deprecation_age::run(
            context.all_fingerprints,
            context.root,
            &config.detector_profile,
        ),
        GenericDetectorRunner::DeadGuard => {
            dead_guard::run(context.per_file_fingerprints, context.root, config)
        }
        GenericDetectorRunner::RequestedDetectors => {
            requested_detectors::run(context.all_fingerprints, config)
        }
        GenericDetectorRunner::ConfigKeyUsage => {
            config_key_usage::run(context.all_fingerprints, &config.config_key_usage.rules)
        }
        GenericDetectorRunner::OutputCapture => {
            unbounded_output_capture::run(context.per_file_fingerprints)
        }
        GenericDetectorRunner::CoreBoundaryLeaks => source_policy::run(
            context.policy_fingerprints,
            &config.core_boundary_leaks.to_source_policy_rules(),
        ),
        GenericDetectorRunner::SourcePolicy => {
            source_policy::run(context.policy_fingerprints, &config.source_policies)
        }
        GenericDetectorRunner::MutatingResourceAccess => mutating_resource_access::run(
            context.per_file_fingerprints,
            &config.mutating_resource_access,
        ),
        GenericDetectorRunner::RedirectValidation => {
            redirect_validation::run(context.per_file_fingerprints, &config.redirect_validation)
        }
        GenericDetectorRunner::GlobalEnvGuard => global_env_guard::run(context.all_fingerprints),
        GenericDetectorRunner::ParallelRunnerSetup => {
            parallel_runner_setup::run(context.all_fingerprints)
        }
        GenericDetectorRunner::RemoteExecutionPreflight => remote_execution_preflight::run(
            context.all_fingerprints,
            &config.remote_execution_safety,
        ),
        GenericDetectorRunner::EnumDispatchContracts => match context.source_snapshot {
            Some(snapshot) => enum_dispatch_contracts::run(snapshot),
            None => Vec::new(),
        },
        GenericDetectorRunner::PublicRegistryExposure => public_registry_exposure::run(
            context.all_fingerprints,
            &config.public_registry_exposure,
        ),
        GenericDetectorRunner::CommandStatusContracts => {
            command_status_contracts::run(context.root, &config.command_status_contracts)
        }
        GenericDetectorRunner::ThinCommandAdapter => {
            thin_command_adapter::run(context.root, &config.thin_command_adapter)
        }
    }
}

/// Dead-code analysis fingerprints external/component reference files lazily so
/// the (potentially expensive) reference walk only happens when the detector is
/// enabled. The dispatch only invokes this runner for an enabled descriptor, so
/// the walk stays gated exactly as it was when hand-wired in `engine.rs`.
fn run_dead_code(context: &DetectorRunContext<'_>) -> Vec<Finding> {
    let references = context
        .dead_code_references
        .expect("dead-code detector receives repository reference analysis when enabled");
    let ref_fp_refs: Vec<&fingerprint::FileFingerprint> = references
        .external
        .iter()
        .chain(references.component.iter())
        .collect();
    dead_code::analyze_dead_code_with_config(
        context.all_fingerprints,
        &ref_fp_refs,
        context.audit_config,
    )
}

/// Structural test-coverage gap detection. Uses the first installed extension
/// that declares a `test_mapping` for the component, matching the prior
/// hand-wired loop's "first extension wins, then stop" behavior.
fn run_test_coverage(context: &DetectorRunContext<'_>) -> Vec<Finding> {
    let standalone_vacuity = context.test_quality_findings.map_or_else(
        || test_coverage::run_vacuity(context.root),
        test_coverage::run_vacuity_from_shared,
    );
    let Some(comp) = super::component_provider::resolve_by_id(context.component_id) else {
        return standalone_vacuity;
    };
    for ext_id in &comp.extension_ids {
        if let Some(ext_manifest) = super::extension_manifests::load_audit_manifest(ext_id) {
            if let Some(test_mapping) = &ext_manifest.test_mapping {
                let mut findings = standalone_vacuity;
                findings.extend(test_coverage::analyze_test_coverage(
                    context.root,
                    context.all_fingerprints,
                    test_mapping,
                ));
                return findings;
            }
        }
    }
    standalone_vacuity
}

fn extend_descriptor_findings(
    all_findings: &mut Vec<Finding>,
    descriptor: &DetectorDescriptor,
    findings: Vec<Finding>,
) {
    if findings.is_empty() {
        return;
    }

    log_status!(
        "audit",
        "{}: {} finding(s) ({})",
        descriptor.log_label,
        findings.len(),
        descriptor.log_summary
    );
    all_findings.extend(findings);
}

/// One descriptor's completed work, carried back from whichever worker thread
/// ran it.
///
/// Findings and spans travel together and are replayed in descriptor-table order
/// by [`merge_descriptor_outcomes`], so completion order never reaches the audit
/// result or the timing report.
pub(super) struct DescriptorOutcome {
    descriptor: &'static DetectorDescriptor,
    findings: Vec<Finding>,
    spans: Vec<AuditTimingSpan>,
}

/// Run a single descriptor's detector, timed into its own span list.
///
/// Pure with respect to everything it touches: it reads the shared immutable
/// [`DetectorRunContext`] and the plan, and returns owned findings. That is what
/// makes the fan-out below safe without any locking around detector execution.
fn run_one_descriptor(
    plan: &AuditExecutionPlan,
    descriptor: &'static DetectorDescriptor,
    context: &DetectorRunContext<'_>,
) -> DescriptorOutcome {
    let enabled = plan.detector_enabled(descriptor.id);
    let (findings, spans) = match descriptor.runtime {
        DetectorRuntime::Generic(runner) => time_audit_detector_isolated(
            descriptor.timing_id,
            enabled,
            || run_generic_descriptor(runner, context),
            Vec::new,
        ),
        DetectorRuntime::Fingerprint(runner) => time_audit_detector_isolated(
            descriptor.timing_id,
            enabled,
            || run_fingerprint_descriptor(runner, context),
            Vec::new,
        ),
        DetectorRuntime::Root(runner) => time_audit_detector_isolated(
            descriptor.timing_id,
            enabled,
            || run_root_descriptor(runner, context),
            Vec::new,
        ),
        // Filtered out before dispatch by `selected_descriptors`.
        DetectorRuntime::Manual => (Vec::new(), Vec::new()),
    };

    DescriptorOutcome {
        descriptor,
        findings,
        spans,
    }
}

/// The descriptors this dispatch will run, in descriptor-table order.
///
/// `ids = None` selects every data-driven detector (the full-discovery path);
/// `ids = Some(subset)` selects only the listed detectors (the root-only fast
/// path). `Manual` descriptors — the convention pipeline, the multi-pass
/// duplication family, and artifact portability — are sequenced by hand in
/// `engine.rs` and excluded here.
fn selected_descriptors(ids: Option<&[&str]>) -> Vec<&'static DetectorDescriptor> {
    AuditExecutionPlan::descriptors()
        .iter()
        .filter(|descriptor| !matches!(descriptor.runtime, DetectorRuntime::Manual))
        .filter(|descriptor| ids.is_none_or(|ids| ids.contains(&descriptor.id)))
        .collect()
}

/// Run the selected descriptor detectors CONCURRENTLY and return their outcomes
/// in descriptor-table order.
///
/// The detectors are independent by construction — each reads the shared
/// immutable [`DetectorRunContext`] and returns owned findings — and the
/// descriptor table is the only thing that ever ordered them. Determinism is
/// therefore preserved exactly: `map_parallel` discards completion order and
/// returns one outcome per descriptor in table order, and each outcome carries its
/// own spans, so [`merge_descriptor_outcomes`] can replay both without ever
/// observing thread scheduling.
///
/// Split from the merge so a caller running this concurrently with other detector
/// families can still emit ITS log lines in the original sequence: the findings
/// logging happens in the merge, on the caller's thread, after the fan-out.
pub(super) fn descriptor_detector_outcomes(
    plan: &AuditExecutionPlan,
    context: &DetectorRunContext<'_>,
    ids: Option<&[&str]>,
) -> Vec<DescriptorOutcome> {
    let descriptors = selected_descriptors(ids);
    super::parallel::map_parallel(&descriptors, |descriptor| {
        run_one_descriptor(plan, descriptor, context)
    })
}

/// Replay descriptor outcomes into the audit's timing report and findings vector,
/// in the descriptor-table order they arrived in.
pub(super) fn merge_descriptor_outcomes(
    timing: &mut AuditTiming,
    all_findings: &mut Vec<Finding>,
    outcomes: Vec<DescriptorOutcome>,
) {
    for outcome in outcomes {
        timing.spans.extend(outcome.spans);
        extend_descriptor_findings(all_findings, outcome.descriptor, outcome.findings);
    }
}

/// Drive the descriptor table. `ids = None` runs every data-driven detector
/// (the full-discovery path); `ids = Some(subset)` runs only the listed
/// detectors (the root-only fast path). `Manual` descriptors — the convention
/// pipeline, the multi-pass duplication family, and artifact portability — are
/// sequenced by hand in `engine.rs` and skipped here.
pub(super) fn run_descriptor_detectors(
    plan: &AuditExecutionPlan,
    timing: &mut AuditTiming,
    all_findings: &mut Vec<Finding>,
    context: &DetectorRunContext<'_>,
    ids: Option<&[&str]>,
) {
    let outcomes = descriptor_detector_outcomes(plan, context, ids);
    merge_descriptor_outcomes(timing, all_findings, outcomes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditProfile;
    use homeboy_audit_contract::{
        AggregateDefinitionFact, AggregateFieldFact, AggregateProjectionFact, DecisionBranchFact,
        FactLocation, PolicyDecisionSink, PolicyFlowConfig, PolicyFlowRule, ProjectionFieldFact,
    };

    /// A directory containing a single file well past the structural line
    /// threshold, so the `structural` detector emits exactly one `GodFile`.
    fn god_file_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut content = String::new();
        for i in 0..1600 {
            content.push_str(&format!("// line {i}\n"));
        }
        std::fs::write(dir.join("big.rs"), content).unwrap();
        dir
    }

    /// Fingerprints that describe a policy aggregate, a lossy projection of it,
    /// and the decision that consumes the projection.
    fn policy_flow_fingerprints() -> Vec<fingerprint::FileFingerprint> {
        let source = fingerprint::FileFingerprint {
            relative_path: "src/policy.ext".to_string(),
            aggregate_definitions: vec![AggregateDefinitionFact {
                type_id: "domain::Policy".to_string(),
                fields: vec![AggregateFieldFact {
                    name: "threshold".to_string(),
                    type_id: None,
                }],
                location: FactLocation { line: 1, column: 1 },
            }],
            ..Default::default()
        };
        let projection = fingerprint::FileFingerprint {
            relative_path: "src/project.ext".to_string(),
            aggregate_projections: vec![AggregateProjectionFact {
                source_type_id: "domain::Policy".to_string(),
                target_type_id: "domain::Carrier".to_string(),
                callable_id: "domain::project".to_string(),
                field_mappings: vec![ProjectionFieldFact {
                    source_field: "id".to_string(),
                    target_field: "id".to_string(),
                }],
                location: FactLocation { line: 4, column: 1 },
            }],
            ..Default::default()
        };
        let decision = fingerprint::FileFingerprint {
            relative_path: "src/decide.ext".to_string(),
            decision_branches: vec![DecisionBranchFact {
                callable_id: "domain::decide".to_string(),
                domain_type_id: "domain::Severity".to_string(),
                discriminant_id: "severity".to_string(),
                location: FactLocation { line: 7, column: 1 },
            }],
            ..Default::default()
        };
        vec![source, projection, decision]
    }

    /// The rule that makes [`policy_flow_fingerprints`] a lossy projection.
    fn policy_flow_config() -> AuditConfig {
        AuditConfig {
            policy_flow: PolicyFlowConfig {
                rules: vec![PolicyFlowRule {
                    id: "policy".to_string(),
                    source_type_id: "domain::Policy".to_string(),
                    policy_fields: vec!["threshold".to_string()],
                    authoritative_method_id: "domain::Policy::allows".to_string(),
                    decision_sinks: vec![PolicyDecisionSink {
                        carrier_type_id: "domain::Carrier".to_string(),
                        callable_id: "domain::decide".to_string(),
                        domain_type_id: "domain::Severity".to_string(),
                    }],
                    convention: "policy_flow".to_string(),
                    severity: "warning".to_string(),
                }],
            },
            ..Default::default()
        }
    }

    /// Only the three hand-sequenced families remain `Manual`; every other
    /// detector is dispatched through the data-driven runtime. This guards
    /// against a new detector being added as a hand-wired `engine.rs` block.
    #[test]
    fn only_special_families_remain_manual() {
        let manual: Vec<&str> = AuditExecutionPlan::descriptors()
            .iter()
            .filter(|descriptor| matches!(descriptor.runtime, DetectorRuntime::Manual))
            .map(|descriptor| descriptor.id)
            .collect();

        assert_eq!(
            manual,
            vec!["conventions", "duplication", "artifact_portability"]
        );
    }

    /// A detector that used to be hand-wired in `engine.rs` (`structural`) now
    /// flows through `run_descriptor_detectors` end to end: the descriptor's
    /// `Generic` runtime is dispatched, the finding is collected, and timing is
    /// recorded — none of which touches a per-detector block.
    #[test]
    fn migrated_detector_runs_via_data_driven_dispatch() {
        let dir = god_file_dir("homeboy_descriptor_dispatch");

        // Confirm the descriptor is genuinely data-driven, not Manual.
        let structural = AuditExecutionPlan::descriptors()
            .iter()
            .find(|descriptor| descriptor.id == "structural")
            .expect("structural descriptor");
        assert_eq!(
            structural.runtime,
            DetectorRuntime::Generic(GenericDetectorRunner::Structural)
        );

        let audit_config = AuditConfig::default();
        let context = DetectorRunContext {
            root: &dir,
            component_id: "fixture-component",
            audit_config: &audit_config,
            all_fingerprints: &[],
            per_file_fingerprints: &[],
            policy_fingerprints: &[],
            source_snapshot: None,
            dead_code_references: None,
            test_quality_findings: None,
        };

        let plan = AuditExecutionPlan::from_profile_and_filters(AuditProfile::Full, &[], &[]);
        let mut timing = AuditTiming::default();
        let mut findings = Vec::new();
        run_descriptor_detectors(
            &plan,
            &mut timing,
            &mut findings,
            &context,
            Some(&["structural"]),
        );

        assert!(
            findings
                .iter()
                .any(|finding| finding.file.contains("big.rs")),
            "structural detector should emit a god-file finding via the data-driven dispatch"
        );
        assert!(
            timing
                .spans
                .iter()
                .any(|span| span.id == "detector.structural" && span.status == "ok"),
            "dispatch should record the structural timing span"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn policy_flow_runs_via_data_driven_dispatch() {
        let dir = std::env::temp_dir().join(format!(
            "homeboy_policy_flow_descriptor_dispatch_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let owned = policy_flow_fingerprints();
        let fingerprints: Vec<&fingerprint::FileFingerprint> = owned.iter().collect();
        let audit_config = policy_flow_config();
        let context = DetectorRunContext {
            root: &dir,
            component_id: "fixture-component",
            audit_config: &audit_config,
            all_fingerprints: &fingerprints,
            per_file_fingerprints: &fingerprints,
            policy_fingerprints: &fingerprints,
            source_snapshot: None,
            dead_code_references: None,
            test_quality_findings: None,
        };
        let plan = AuditExecutionPlan::from_profile_and_filters(AuditProfile::Full, &[], &[]);
        let mut timing = AuditTiming::default();
        let mut findings = Vec::new();

        run_descriptor_detectors(
            &plan,
            &mut timing,
            &mut findings,
            &context,
            Some(&["policy_flow"]),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, crate::AuditFinding::LossyPolicyProjection);
        assert!(timing
            .spans
            .iter()
            .any(|span| span.id == "detector.policy_flow" && span.status == "ok"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The detector fan-out must not leak thread scheduling into the audit's
    /// output. Two detectors that both produce findings are dispatched together,
    /// and BOTH the findings vector and the timing span list have to come back in
    /// descriptor-table order — `structural` (table row 2) ahead of `policy_flow`
    /// (table row 31) — on every run rather than on the lucky ones.
    ///
    /// The requested ids are deliberately passed in the OPPOSITE order to prove
    /// the caller's argument order is not what produces the result order: the
    /// descriptor table is, exactly as it was when the dispatch was a serial loop.
    #[test]
    fn parallel_dispatch_preserves_descriptor_table_order() {
        let dir = god_file_dir("homeboy_descriptor_order");
        let owned = policy_flow_fingerprints();
        let fingerprints: Vec<&fingerprint::FileFingerprint> = owned.iter().collect();
        let audit_config = policy_flow_config();
        let context = DetectorRunContext {
            root: &dir,
            component_id: "fixture-component",
            audit_config: &audit_config,
            all_fingerprints: &fingerprints,
            per_file_fingerprints: &fingerprints,
            policy_fingerprints: &fingerprints,
            source_snapshot: None,
            dead_code_references: None,
            test_quality_findings: None,
        };
        let plan = AuditExecutionPlan::from_profile_and_filters(AuditProfile::Full, &[], &[]);

        // One pass can agree with the table by luck. Repeating it cannot.
        for attempt in 0..25 {
            let mut timing = AuditTiming::default();
            let mut findings = Vec::new();
            run_descriptor_detectors(
                &plan,
                &mut timing,
                &mut findings,
                &context,
                Some(&["policy_flow", "structural"]),
            );

            let span_ids: Vec<&str> = timing.spans.iter().map(|span| span.id.as_str()).collect();
            assert_eq!(
                span_ids,
                vec!["detector.structural", "detector.policy_flow"],
                "attempt {attempt}: spans must follow descriptor-table order, not completion order"
            );

            let kinds: Vec<crate::AuditFinding> = findings
                .iter()
                .map(|finding| finding.kind.clone())
                .collect();
            assert_eq!(
                kinds,
                vec![
                    crate::AuditFinding::GodFile,
                    crate::AuditFinding::LossyPolicyProjection
                ],
                "attempt {attempt}: findings must be merged in descriptor-table order"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
