use super::AuditFinding;
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuditExecutionPlan {
    /// Authoritative detector-family execution contract. `run_*` helpers below
    /// derive from this plan so callers do not maintain parallel selector state.
    pub(crate) plan: HomeboyPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditProfile {
    Full,
    Pr,
    Architecture,
}

impl AuditProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Pr => "pr",
            Self::Architecture => "architecture",
        }
    }

    fn includes_detector(self, family: &DetectorDescriptor) -> bool {
        match self {
            Self::Full => true,
            Self::Pr => matches!(
                family.id,
                "structural"
                    | "layer_ownership"
                    | "test_topology"
                    | "test_wiring"
                    | "docs"
                    | "command_status_contracts"
                    | "thin_command_adapter"
            ),
            Self::Architecture => matches!(
                family.id,
                "structural"
                    | "layer_ownership"
                    | "docs"
                    | "command_status_contracts"
                    | "thin_command_adapter"
            ),
        }
    }
}

impl std::str::FromStr for AuditProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "full" => Ok(Self::Full),
            "pr" => Ok(Self::Pr),
            "architecture" => Ok(Self::Architecture),
            _ => Err(format!(
                "unknown audit profile '{value}' (expected full, pr, or architecture)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectorAccess {
    Discovery,
    RootOnly,
}

impl DetectorAccess {
    fn requires_discovery(self) -> bool {
        matches!(self, Self::Discovery)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectorRuntime {
    /// Detectors the engine still sequences by hand because they have a
    /// non-uniform shape: the convention pipeline (`conventions`), the
    /// multi-pass `duplication` family (five timing spans plus the
    /// `duplicate_groups` side output), and `artifact_portability` (logs scan
    /// statistics even when it finds nothing). Everything else is data-driven.
    Manual,
    /// Single-closure detectors driven entirely by the descriptor table via
    /// [`super::descriptor_runtime::run_descriptor_detectors`].
    Generic(GenericDetectorRunner),
    Fingerprint(FingerprintDetectorRunner),
    Root(RootDetectorRunner),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FingerprintDetectorRunner {
    ShadowModules,
    FacadePassthrough,
    LiteralShapes,
    SharedScaffolding,
    AggregateConstruction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootDetectorRunner {
    TestTopology,
    TestWiring,
}

/// Identifies which single-closure detector a [`DetectorRuntime::Generic`]
/// descriptor dispatches to. The descriptor table is the single registration
/// site; [`super::descriptor_runtime`] owns the one-line invocation per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenericDetectorRunner {
    Structural,
    DeadCode,
    CommentHygiene,
    TestCoverage,
    LayerOwnership,
    Docs,
    CompilerWarnings,
    WrapperInference,
    FieldPatterns,
    DeprecationAge,
    DeadGuard,
    RequestedDetectors,
    ConfigKeyUsage,
    OutputCapture,
    CoreBoundaryLeaks,
    SourcePolicy,
    MutatingResourceAccess,
    RedirectValidation,
    GlobalEnvGuard,
    ParallelRunnerSetup,
    RemoteExecutionPreflight,
    EnumDispatchContracts,
    PublicRegistryExposure,
    CommandStatusContracts,
    ThinCommandAdapter,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DetectorDescriptor {
    pub(crate) id: &'static str,
    pub(crate) findings: &'static [AuditFinding],
    pub(crate) access: DetectorAccess,
    pub(crate) runtime: DetectorRuntime,
    pub(crate) timing_id: &'static str,
    pub(crate) log_label: &'static str,
    pub(crate) log_summary: &'static str,
}

const DETECTOR_DESCRIPTORS: &[DetectorDescriptor] = &[
    DetectorDescriptor {
        id: "conventions",
        findings: &[
            AuditFinding::MissingMethod,
            AuditFinding::ExtraMethod,
            AuditFinding::MissingRegistration,
            AuditFinding::DifferentRegistration,
            AuditFinding::MissingInterface,
            AuditFinding::NamingMismatch,
            AuditFinding::SignatureMismatch,
            AuditFinding::NamespaceMismatch,
            AuditFinding::MissingImport,
        ],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.conventions",
        log_label: "Conventions",
        log_summary: "convention outliers",
    },
    DetectorDescriptor {
        id: "structural",
        findings: &[
            AuditFinding::GodFile,
            AuditFinding::HighItemCount,
            AuditFinding::DirectorySprawl,
        ],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::Structural),
        timing_id: "detector.structural",
        log_label: "Structural",
        log_summary: "god files, high item counts",
    },
    DetectorDescriptor {
        id: "duplication",
        findings: &[
            AuditFinding::DuplicateFunction,
            AuditFinding::IntraMethodDuplicate,
            AuditFinding::NearDuplicate,
            AuditFinding::ParallelImplementation,
        ],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.duplication",
        log_label: "Duplication",
        log_summary: "duplicate implementations",
    },
    DetectorDescriptor {
        id: "dead_code",
        findings: &[
            AuditFinding::UnusedParameter,
            AuditFinding::IgnoredParameter,
            AuditFinding::DeadCodeMarker,
            AuditFinding::UnreferencedExport,
            AuditFinding::OrphanedInternal,
        ],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::DeadCode),
        timing_id: "detector.dead_code",
        log_label: "Dead code",
        log_summary: "unused params, unreferenced exports, orphaned internals",
    },
    DetectorDescriptor {
        id: "comment_hygiene",
        findings: &[
            AuditFinding::TodoMarker,
            AuditFinding::LegacyComment,
            // The comment_hygiene runner also delegates to upstream_workaround,
            // which emits UpstreamWorkaround. It must be declared here so
            // `--only upstream_workaround` keeps this family enabled instead of
            // silently disabling the detector.
            AuditFinding::UpstreamWorkaround,
        ],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::CommentHygiene),
        timing_id: "detector.comment_hygiene",
        log_label: "Comment hygiene",
        log_summary: "TODO/FIXME/HACK markers, stale phrasing",
    },
    DetectorDescriptor {
        id: "test_coverage",
        findings: &[
            AuditFinding::MissingTestFile,
            AuditFinding::MissingTestMethod,
            AuditFinding::OrphanedTest,
            AuditFinding::VacuousTest,
        ],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::TestCoverage),
        timing_id: "detector.test_coverage",
        log_label: "Test coverage",
        log_summary: "missing test files, uncovered methods, orphaned tests",
    },
    DetectorDescriptor {
        id: "layer_ownership",
        findings: &[AuditFinding::LayerOwnershipViolation],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::LayerOwnership),
        timing_id: "detector.layer_ownership",
        log_label: "Layer ownership",
        log_summary: "architecture ownership violations",
    },
    DetectorDescriptor {
        id: "test_topology",
        findings: &[
            AuditFinding::InlineTestModule,
            AuditFinding::ScatteredTestFile,
            AuditFinding::VacuousTest,
        ],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Root(RootDetectorRunner::TestTopology),
        timing_id: "detector.test_topology",
        log_label: "Test topology",
        log_summary: "inline/scattered test placement",
    },
    DetectorDescriptor {
        id: "test_wiring",
        findings: &[AuditFinding::UnwiredNestedRustTest],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Root(RootDetectorRunner::TestWiring),
        timing_id: "detector.test_wiring",
        log_label: "Nested test wiring",
        log_summary: "nested tests not wired into the test runner",
    },
    DetectorDescriptor {
        id: "docs",
        findings: &[
            AuditFinding::BrokenDocReference,
            AuditFinding::UndocumentedFeature,
            AuditFinding::StaleDocReference,
        ],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::Docs),
        timing_id: "detector.docs",
        log_label: "Docs",
        log_summary: "broken references, stale paths",
    },
    DetectorDescriptor {
        id: "compiler_warnings",
        findings: &[AuditFinding::CompilerWarning],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::CompilerWarnings),
        timing_id: "detector.compiler_warnings",
        log_label: "Compiler warnings",
        log_summary: "dead code, unused imports, unused variables",
    },
    DetectorDescriptor {
        id: "wrapper_inference",
        findings: &[AuditFinding::MissingWrapperDeclaration],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::WrapperInference),
        timing_id: "detector.wrapper_inference",
        log_label: "Wrapper inference",
        log_summary: "missing wrapper declarations",
    },
    DetectorDescriptor {
        id: "shadow_modules",
        findings: &[AuditFinding::ShadowModule],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Fingerprint(FingerprintDetectorRunner::ShadowModules),
        timing_id: "detector.shadow_modules",
        log_label: "Shadow modules",
        log_summary: "duplicate directory structures",
    },
    DetectorDescriptor {
        id: "field_patterns",
        findings: &[AuditFinding::RepeatedFieldPattern],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::FieldPatterns),
        timing_id: "detector.field_patterns",
        log_label: "Field patterns",
        log_summary: "repeated struct fields",
    },
    DetectorDescriptor {
        id: "facade_passthrough",
        findings: &[AuditFinding::FacadePassthrough],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Fingerprint(FingerprintDetectorRunner::FacadePassthrough),
        timing_id: "detector.facade_passthrough",
        log_label: "Facade passthrough",
        log_summary: "thin wrapper classes",
    },
    DetectorDescriptor {
        id: "literal_shapes",
        findings: &[AuditFinding::RepeatedLiteralShape],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Fingerprint(FingerprintDetectorRunner::LiteralShapes),
        timing_id: "detector.literal_shapes",
        log_label: "Literal shapes",
        log_summary: "repeated inline array literals",
    },
    DetectorDescriptor {
        id: "deprecation_age",
        findings: &[AuditFinding::DeprecationAge],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::DeprecationAge),
        timing_id: "detector.deprecation_age",
        log_label: "Deprecation age",
        log_summary: "stale @deprecated tags",
    },
    DetectorDescriptor {
        id: "dead_guard",
        findings: &[AuditFinding::DeadGuard],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::DeadGuard),
        timing_id: "detector.dead_guard",
        log_label: "Dead guards",
        log_summary: "guards on guaranteed-available symbols",
    },
    DetectorDescriptor {
        id: "requested_detectors",
        findings: &[
            AuditFinding::JsonLikeExactMatch,
            AuditFinding::ConstantBackedSlugLiteral,
            AuditFinding::OptionScopeDrift,
            AuditFinding::ProxyScopeDrift,
            AuditFinding::ConfigRoundtripAsymmetry,
        ],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::RequestedDetectors),
        timing_id: "detector.requested_detectors",
        log_label: "Requested detectors",
        log_summary: "extension rule packs",
    },
    DetectorDescriptor {
        id: "core_boundary_leaks",
        findings: &[AuditFinding::CoreBoundaryLeak],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::CoreBoundaryLeaks),
        timing_id: "detector.core_boundary_leaks",
        log_label: "Core boundary leaks",
        log_summary: "configured ecosystem terms in core source",
    },
    DetectorDescriptor {
        id: "source_policy",
        findings: &[AuditFinding::SourcePolicyViolation],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::SourcePolicy),
        timing_id: "detector.source_policy",
        log_label: "Source policy",
        log_summary: "configured source policy rules",
    },
    DetectorDescriptor {
        id: "mutating_resource_access",
        findings: &[AuditFinding::MutatingResourceAccess],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::MutatingResourceAccess),
        timing_id: "detector.mutating_resource_access",
        log_label: "Mutating resource access",
        log_summary: "resource mutations without configured access checks",
    },
    DetectorDescriptor {
        id: "redirect_validation",
        findings: &[AuditFinding::RedirectValidation],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::RedirectValidation),
        timing_id: "detector.redirect_validation",
        log_label: "Redirect validation",
        log_summary: "request-derived redirects without dominating validation",
    },
    DetectorDescriptor {
        id: "global_env_guard",
        findings: &[AuditFinding::GlobalEnvMutationGuard],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::GlobalEnvGuard),
        timing_id: "detector.global_env_guard",
        log_label: "Global env guards",
        log_summary: "test env mutation without shared guard",
    },
    DetectorDescriptor {
        id: "shared_scaffolding",
        findings: &[AuditFinding::SharedScaffolding],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Fingerprint(FingerprintDetectorRunner::SharedScaffolding),
        timing_id: "detector.shared_scaffolding",
        log_label: "Shared scaffolding",
        log_summary: "candidate base class groups",
    },
    DetectorDescriptor {
        id: "parallel_runner_setup",
        findings: &[AuditFinding::ParallelRunnerSetup],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::ParallelRunnerSetup),
        timing_id: "detector.parallel_runner_setup",
        log_label: "Parallel runner setup",
        log_summary: "duplicated execution contract setup",
    },
    DetectorDescriptor {
        id: "remote_execution_preflight",
        findings: &[AuditFinding::RemoteExecutionPreflight],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::RemoteExecutionPreflight),
        timing_id: "detector.remote_execution_preflight",
        log_label: "Remote execution preflight",
        log_summary: "remote execution path/artifact parity gaps",
    },
    DetectorDescriptor {
        id: "enum_dispatch_contracts",
        findings: &[AuditFinding::RepeatedEnumDispatchContract],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::EnumDispatchContracts),
        timing_id: "detector.enum_dispatch_contracts",
        log_label: "Enum dispatch contracts",
        log_summary: "repeated exhaustive enum matches",
    },
    DetectorDescriptor {
        id: "aggregate_construction",
        findings: &[AuditFinding::DirectAggregateConstruction],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Fingerprint(FingerprintDetectorRunner::AggregateConstruction),
        timing_id: "detector.aggregate_construction",
        log_label: "Aggregate construction",
        log_summary: "direct literals bypass construction seams",
    },
    DetectorDescriptor {
        id: "public_registry_exposure",
        findings: &[AuditFinding::PublicRegistryExposure],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::PublicRegistryExposure),
        timing_id: "detector.public_registry_exposure",
        log_label: "Public registry exposure",
        log_summary: "public metadata routes bypassing resolvers",
    },
    DetectorDescriptor {
        id: "config_key_usage",
        findings: &[AuditFinding::WriteOnlyConfigKey],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::ConfigKeyUsage),
        timing_id: "detector.config_key_usage",
        log_label: "Config key usage",
        log_summary: "write/accessor evidence without production reads",
    },
    DetectorDescriptor {
        id: "artifact_portability",
        findings: &[AuditFinding::NonPortableArtifactPath],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.artifact_portability",
        log_label: "Artifact portability",
        log_summary: "non-portable artifact evidence paths",
    },
    DetectorDescriptor {
        id: "output_capture",
        findings: &[AuditFinding::UnboundedOutputCapture],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::OutputCapture),
        timing_id: "detector.output_capture",
        log_label: "Output capture",
        log_summary: "unbounded stdout/stderr capture",
    },
    DetectorDescriptor {
        id: "command_status_contracts",
        findings: &[AuditFinding::CommandStatusContractViolation],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::CommandStatusContracts),
        timing_id: "detector.command_status_contracts",
        log_label: "Command status contracts",
        log_summary: "inconsistent no-op/dry-run status fields",
    },
    DetectorDescriptor {
        id: "thin_command_adapter",
        findings: &[AuditFinding::ThinCommandAdapterViolation],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Generic(GenericDetectorRunner::ThinCommandAdapter),
        timing_id: "detector.thin_command_adapter",
        log_label: "Thin command adapters",
        log_summary: "command modules accumulating orchestration logic",
    },
];

impl AuditExecutionPlan {
    pub(crate) fn full() -> Self {
        Self::from_profile_and_filters(AuditProfile::Full, &[], &[])
    }

    #[cfg(test)]
    pub(crate) fn from_filters(only: &[AuditFinding], exclude: &[AuditFinding]) -> Self {
        Self::from_profile_and_filters(AuditProfile::Full, only, exclude)
    }

    pub(crate) fn from_profile_and_filters(
        profile: AuditProfile,
        only: &[AuditFinding],
        exclude: &[AuditFinding],
    ) -> Self {
        let mode = if profile == AuditProfile::Full && (!only.is_empty() || !exclude.is_empty()) {
            "filtered"
        } else {
            profile.as_str()
        };

        Self::from_enabled_families(mode, |family| {
            profile.includes_detector(family) && family_enabled(only, exclude, family.findings)
        })
    }

    fn from_enabled_families(mode: &str, is_enabled: impl Fn(&DetectorDescriptor) -> bool) -> Self {
        let steps: Vec<PlanStep> = DETECTOR_DESCRIPTORS
            .iter()
            .map(|family| detector_step(family.id, is_enabled(family)))
            .collect();

        Self {
            plan: HomeboyPlan::builder_for_description(PlanKind::Audit, "audit execution")
                .mode(mode)
                .steps(steps)
                .summarize_disabled_as_skipped()
                .build(),
        }
    }

    pub(crate) fn detector_enabled(&self, id: &str) -> bool {
        let step_id = detector_step_id(id);
        self.plan
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .is_some_and(|step| step.status == PlanStepStatus::Ready)
    }

    pub(crate) fn requires_discovery(&self) -> bool {
        DETECTOR_DESCRIPTORS
            .iter()
            .any(|family| family.access.requires_discovery() && self.detector_enabled(family.id))
    }

    pub(crate) fn descriptors() -> &'static [DetectorDescriptor] {
        DETECTOR_DESCRIPTORS
    }
}

fn detector_step_id(name: &str) -> String {
    format!("audit.{name}")
}

fn detector_step(name: &str, enabled: bool) -> PlanStep {
    let builder = PlanStep::builder(
        detector_step_id(name),
        format!("audit.detector.{name}"),
        if enabled {
            PlanStepStatus::Ready
        } else {
            PlanStepStatus::Disabled
        },
    )
    .label(name.replace('_', " "));

    if enabled {
        builder
    } else {
        builder.skip_reason("filtered")
    }
    .build()
}

fn family_enabled(
    only: &[AuditFinding],
    exclude: &[AuditFinding],
    emitted: &[AuditFinding],
) -> bool {
    let requested = only.is_empty() || emitted.iter().any(|kind| only.contains(kind));
    let fully_excluded = emitted.iter().all(|kind| exclude.contains(kind));

    requested && !fully_excluded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detector_cluster_uses_root_descriptor_runtime() {
        let runtimes: Vec<_> = AuditExecutionPlan::descriptors()
            .iter()
            .filter(|descriptor| ["test_topology", "test_wiring"].contains(&descriptor.id))
            .map(|descriptor| descriptor.runtime)
            .collect();

        assert_eq!(
            runtimes,
            vec![
                DetectorRuntime::Root(RootDetectorRunner::TestTopology),
                DetectorRuntime::Root(RootDetectorRunner::TestWiring),
            ]
        );
    }

    #[test]
    fn pr_profile_uses_root_only_detector_families() {
        let plan = AuditExecutionPlan::from_profile_and_filters(AuditProfile::Pr, &[], &[]);

        assert!(!plan.requires_discovery());
        assert!(plan.detector_enabled("structural"));
        assert!(plan.detector_enabled("test_topology"));
        assert!(plan.detector_enabled("command_status_contracts"));
        assert!(plan.detector_enabled("thin_command_adapter"));
        assert!(!plan.detector_enabled("duplication"));
        assert!(!plan.detector_enabled("dead_code"));
        assert!(!plan.detector_enabled("source_policy"));
        assert!(!plan.detector_enabled("compiler_warnings"));
    }

    #[test]
    fn profile_filters_are_applied_within_profile_scope() {
        let plan = AuditExecutionPlan::from_profile_and_filters(
            AuditProfile::Pr,
            &[AuditFinding::DuplicateFunction],
            &[],
        );

        assert!(!plan.detector_enabled("structural"));
        assert!(!plan.detector_enabled("duplication"));
        assert!(!plan.requires_discovery());
    }

    #[test]
    fn only_upstream_workaround_keeps_comment_hygiene_enabled() {
        // The comment_hygiene runner delegates to upstream_workaround, so an
        // `--only upstream_workaround` filter must keep the family enabled.
        // Regression: the descriptor previously omitted UpstreamWorkaround,
        // which silently disabled the detector under this filter.
        let plan = AuditExecutionPlan::from_filters(&[AuditFinding::UpstreamWorkaround], &[]);

        assert!(plan.detector_enabled("comment_hygiene"));
        assert!(!plan.detector_enabled("dead_code"));
    }

    #[test]
    fn excluding_all_comment_hygiene_findings_disables_the_family() {
        let plan = AuditExecutionPlan::from_filters(
            &[],
            &[
                AuditFinding::TodoMarker,
                AuditFinding::LegacyComment,
                AuditFinding::UpstreamWorkaround,
            ],
        );

        assert!(!plan.detector_enabled("comment_hygiene"));
    }
}
