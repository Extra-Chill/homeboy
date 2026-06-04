use super::detectors::{
    aggregate_construction, facade_passthrough, repeated_literal_shape, shared_scaffolding,
};
use super::{
    fingerprint, shadow_modules, time_audit_detector, AuditExecutionPlan, AuditTiming,
    DetectorDescriptor, DetectorRuntime, Finding, FingerprintDetectorRunner,
};

pub(super) struct DetectorRunContext<'a> {
    pub(super) all_fingerprints: &'a [&'a fingerprint::FileFingerprint],
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
        FingerprintDetectorRunner::LiteralShapes => {
            repeated_literal_shape::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::SharedScaffolding => {
            shared_scaffolding::run(context.all_fingerprints)
        }
        FingerprintDetectorRunner::AggregateConstruction => {
            aggregate_construction::run(context.all_fingerprints)
        }
    }
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

pub(super) fn run_descriptor_detectors(
    plan: &AuditExecutionPlan,
    timing: &mut AuditTiming,
    all_findings: &mut Vec<Finding>,
    context: &DetectorRunContext<'_>,
    detector_ids: &[&str],
) {
    for descriptor in AuditExecutionPlan::descriptors() {
        if !detector_ids.contains(&descriptor.id) {
            continue;
        }

        let DetectorRuntime::Fingerprint(runner) = descriptor.runtime else {
            continue;
        };

        let findings = time_audit_detector(
            timing,
            descriptor.timing_id,
            plan.detector_enabled(descriptor.id),
            || run_fingerprint_descriptor(runner, context),
            Vec::new,
        );
        extend_descriptor_findings(all_findings, descriptor, findings);
    }
}
