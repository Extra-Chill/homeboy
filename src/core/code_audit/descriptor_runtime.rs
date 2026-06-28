use super::detectors::layer_ownership::run as run_layer_ownership;
use super::detectors::{
    aggregate_construction, command_status_contracts, config_key_usage, dead_guard,
    deprecation_age, enum_dispatch_contracts, facade_passthrough, field_patterns, global_env_guard,
    mutating_resource_access, parallel_runner_setup, public_registry_exposure, redirect_validation,
    remote_execution_preflight, repeated_literal_shape, requested_detectors, shared_scaffolding,
    source_policy, test_coverage, test_topology, test_wiring, thin_command_adapter,
    unbounded_output_capture, wrapper_inference,
};
use super::doc_drift::detect_doc_drift;
use super::reference::{fingerprint_component_reference_files, fingerprint_reference_paths};
use super::{
    comment_hygiene, compiler_warnings, dead_code, fingerprint, shadow_modules, structural,
    time_audit_detector, AuditExecutionPlan, AuditTiming, DetectorDescriptor, DetectorRuntime,
    Finding, FingerprintDetectorRunner, GenericDetectorRunner, RootDetectorRunner,
};
use crate::core::component::{self, AuditConfig};
use crate::core::engine::codebase_scan::CodebaseSnapshot;
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
    /// Shared source snapshot for whole-tree detectors. `None` only on the
    /// root-only fast path when no snapshot-backed detector is enabled.
    pub(super) source_snapshot: Option<&'a CodebaseSnapshot>,
    /// External reference codebases included in dead-code cross-referencing.
    pub(super) reference_paths: &'a [String],
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
        FingerprintDetectorRunner::SharedScaffolding => {
            shared_scaffolding::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::AggregateConstruction => {
            aggregate_construction::run(context.all_fingerprints)
        }
    }
}

fn run_root_descriptor(
    runner: RootDetectorRunner,
    context: &DetectorRunContext<'_>,
) -> Vec<Finding> {
    match runner {
        RootDetectorRunner::TestTopology => test_topology::run(context.root),
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
            Some(snapshot) => {
                structural::analyze_snapshot(context.root, snapshot, &config.language_grammars)
            }
            None => structural::analyze_structure(context.root, &config.language_grammars),
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
        GenericDetectorRunner::FieldPatterns => match context.source_snapshot {
            Some(snapshot) => field_patterns::run(snapshot, &config.detector_profile),
            None => Vec::new(),
        },
        GenericDetectorRunner::DeprecationAge => {
            deprecation_age::run(context.all_fingerprints, context.root, &config.detector_profile)
        }
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
            context.per_file_fingerprints,
            &config.core_boundary_leaks.to_source_policy_rules(),
        ),
        GenericDetectorRunner::SourcePolicy => {
            source_policy::run(context.per_file_fingerprints, &config.source_policies)
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
        GenericDetectorRunner::RemoteExecutionPreflight => {
            remote_execution_preflight::run(context.all_fingerprints, &config.remote_execution_safety)
        }
        GenericDetectorRunner::EnumDispatchContracts => match context.source_snapshot {
            Some(snapshot) => enum_dispatch_contracts::run(snapshot),
            None => Vec::new(),
        },
        GenericDetectorRunner::PublicRegistryExposure => {
            public_registry_exposure::run(context.all_fingerprints, &config.public_registry_exposure)
        }
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
    let ref_fingerprints = fingerprint_reference_paths(context.reference_paths);
    let component_ref_fingerprints = fingerprint_component_reference_files(context.root);
    let ref_fp_refs: Vec<&fingerprint::FileFingerprint> = ref_fingerprints
        .iter()
        .chain(component_ref_fingerprints.iter())
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
    let Ok(comp) = component::load(context.component_id) else {
        return Vec::new();
    };
    let Some(extensions) = comp.extensions else {
        return Vec::new();
    };
    for ext_id in extensions.keys() {
        if let Ok(ext_manifest) = crate::core::extension::load_extension(ext_id) {
            if let Some(test_mapping) = ext_manifest.test_mapping() {
                return test_coverage::run(context.root, context.all_fingerprints, test_mapping);
            }
        }
    }
    Vec::new()
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
    for descriptor in AuditExecutionPlan::descriptors() {
        if let Some(ids) = ids {
            if !ids.contains(&descriptor.id) {
                continue;
            }
        }

        let findings = match descriptor.runtime {
            DetectorRuntime::Generic(runner) => time_audit_detector(
                timing,
                descriptor.timing_id,
                plan.detector_enabled(descriptor.id),
                || run_generic_descriptor(runner, context),
                Vec::new,
            ),
            DetectorRuntime::Fingerprint(runner) => time_audit_detector(
                timing,
                descriptor.timing_id,
                plan.detector_enabled(descriptor.id),
                || run_fingerprint_descriptor(runner, context),
                Vec::new,
            ),
            DetectorRuntime::Root(runner) => time_audit_detector(
                timing,
                descriptor.timing_id,
                plan.detector_enabled(descriptor.id),
                || run_root_descriptor(runner, context),
                Vec::new,
            ),
            DetectorRuntime::Manual => continue,
        };
        extend_descriptor_findings(all_findings, descriptor, findings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::AuditProfile;

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
        let dir = std::env::temp_dir().join(format!(
            "homeboy_descriptor_dispatch_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A god file well past the structural line threshold.
        let mut content = String::new();
        for i in 0..1600 {
            content.push_str(&format!("// line {i}\n"));
        }
        std::fs::write(dir.join("big.rs"), content).unwrap();

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
            source_snapshot: None,
            reference_paths: &[],
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
}
