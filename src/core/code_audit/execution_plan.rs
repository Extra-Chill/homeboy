use super::AuditFinding;
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuditExecutionPlan {
    /// Authoritative detector-family execution contract. `run_*` helpers below
    /// derive from this plan so callers do not maintain parallel selector state.
    pub(crate) plan: HomeboyPlan,
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
    Manual,
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
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.dead_code",
        log_label: "Dead code",
        log_summary: "unused params, unreferenced exports, orphaned internals",
    },
    DetectorDescriptor {
        id: "comment_hygiene",
        findings: &[AuditFinding::TodoMarker, AuditFinding::LegacyComment],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.test_coverage",
        log_label: "Test coverage",
        log_summary: "missing test files, uncovered methods, orphaned tests",
    },
    DetectorDescriptor {
        id: "layer_ownership",
        findings: &[AuditFinding::LayerOwnershipViolation],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.docs",
        log_label: "Docs",
        log_summary: "broken references, stale paths",
    },
    DetectorDescriptor {
        id: "compiler_warnings",
        findings: &[AuditFinding::CompilerWarning],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.compiler_warnings",
        log_label: "Compiler warnings",
        log_summary: "dead code, unused imports, unused variables",
    },
    DetectorDescriptor {
        id: "wrapper_inference",
        findings: &[AuditFinding::MissingWrapperDeclaration],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.deprecation_age",
        log_label: "Deprecation age",
        log_summary: "stale @deprecated tags",
    },
    DetectorDescriptor {
        id: "dead_guard",
        findings: &[AuditFinding::DeadGuard],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.requested_detectors",
        log_label: "Requested detectors",
        log_summary: "extension rule packs",
    },
    DetectorDescriptor {
        id: "core_boundary_leaks",
        findings: &[AuditFinding::CoreBoundaryLeak],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.core_boundary_leaks",
        log_label: "Core boundary leaks",
        log_summary: "configured ecosystem terms in core source",
    },
    DetectorDescriptor {
        id: "source_policy",
        findings: &[AuditFinding::SourcePolicyViolation],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.source_policy",
        log_label: "Source policy",
        log_summary: "configured source policy rules",
    },
    DetectorDescriptor {
        id: "mutating_resource_access",
        findings: &[AuditFinding::MutatingResourceAccess],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.mutating_resource_access",
        log_label: "Mutating resource access",
        log_summary: "resource mutations without configured access checks",
    },
    DetectorDescriptor {
        id: "redirect_validation",
        findings: &[AuditFinding::RedirectValidation],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.redirect_validation",
        log_label: "Redirect validation",
        log_summary: "request-derived redirects without dominating validation",
    },
    DetectorDescriptor {
        id: "global_env_guard",
        findings: &[AuditFinding::GlobalEnvMutationGuard],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.parallel_runner_setup",
        log_label: "Parallel runner setup",
        log_summary: "duplicated execution contract setup",
    },
    DetectorDescriptor {
        id: "remote_execution_preflight",
        findings: &[AuditFinding::RemoteExecutionPreflight],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.remote_execution_preflight",
        log_label: "Remote execution preflight",
        log_summary: "remote execution path/artifact parity gaps",
    },
    DetectorDescriptor {
        id: "enum_dispatch_contracts",
        findings: &[AuditFinding::RepeatedEnumDispatchContract],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.public_registry_exposure",
        log_label: "Public registry exposure",
        log_summary: "public metadata routes bypassing resolvers",
    },
    DetectorDescriptor {
        id: "config_key_usage",
        findings: &[AuditFinding::WriteOnlyConfigKey],
        access: DetectorAccess::Discovery,
        runtime: DetectorRuntime::Manual,
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
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.output_capture",
        log_label: "Output capture",
        log_summary: "unbounded stdout/stderr capture",
    },
    DetectorDescriptor {
        id: "command_status_contracts",
        findings: &[AuditFinding::CommandStatusContractViolation],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.command_status_contracts",
        log_label: "Command status contracts",
        log_summary: "inconsistent no-op/dry-run status fields",
    },
    DetectorDescriptor {
        id: "thin_command_adapter",
        findings: &[AuditFinding::ThinCommandAdapterViolation],
        access: DetectorAccess::RootOnly,
        runtime: DetectorRuntime::Manual,
        timing_id: "detector.thin_command_adapter",
        log_label: "Thin command adapters",
        log_summary: "command modules accumulating orchestration logic",
    },
];

impl AuditExecutionPlan {
    pub(crate) fn full() -> Self {
        Self::from_enabled_families("full", |family| family_enabled(&[], &[], family.findings))
    }

    pub(crate) fn from_filters(only: &[AuditFinding], exclude: &[AuditFinding]) -> Self {
        if only.is_empty() && exclude.is_empty() {
            return Self::full();
        }

        Self::from_enabled_families("filtered", |family| {
            family_enabled(only, exclude, family.findings)
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

    pub(crate) fn run_structural(&self) -> bool {
        self.detector_enabled("structural")
    }

    pub(crate) fn run_duplication(&self) -> bool {
        self.detector_enabled("duplication")
    }

    pub(crate) fn run_dead_code(&self) -> bool {
        self.detector_enabled("dead_code")
    }

    pub(crate) fn run_comment_hygiene(&self) -> bool {
        self.detector_enabled("comment_hygiene")
    }

    pub(crate) fn run_test_coverage(&self) -> bool {
        self.detector_enabled("test_coverage")
    }

    pub(crate) fn run_layer_ownership(&self) -> bool {
        self.detector_enabled("layer_ownership")
    }

    pub(crate) fn run_docs(&self) -> bool {
        self.detector_enabled("docs")
    }

    pub(crate) fn run_compiler_warnings(&self) -> bool {
        self.detector_enabled("compiler_warnings")
    }

    pub(crate) fn run_wrapper_inference(&self) -> bool {
        self.detector_enabled("wrapper_inference")
    }

    pub(crate) fn run_field_patterns(&self) -> bool {
        self.detector_enabled("field_patterns")
    }

    pub(crate) fn run_deprecation_age(&self) -> bool {
        self.detector_enabled("deprecation_age")
    }

    pub(crate) fn run_dead_guard(&self) -> bool {
        self.detector_enabled("dead_guard")
    }

    pub(crate) fn run_requested_detectors(&self) -> bool {
        self.detector_enabled("requested_detectors")
    }

    pub(crate) fn run_core_boundary_leaks(&self) -> bool {
        self.detector_enabled("core_boundary_leaks")
    }

    pub(crate) fn run_source_policy(&self) -> bool {
        self.detector_enabled("source_policy")
    }

    pub(crate) fn run_mutating_resource_access(&self) -> bool {
        self.detector_enabled("mutating_resource_access")
    }

    pub(crate) fn run_redirect_validation(&self) -> bool {
        self.detector_enabled("redirect_validation")
    }

    pub(crate) fn run_global_env_guard(&self) -> bool {
        self.detector_enabled("global_env_guard")
    }

    pub(crate) fn run_parallel_runner_setup(&self) -> bool {
        self.detector_enabled("parallel_runner_setup")
    }

    pub(crate) fn run_remote_execution_preflight(&self) -> bool {
        self.detector_enabled("remote_execution_preflight")
    }

    pub(crate) fn run_enum_dispatch_contracts(&self) -> bool {
        self.detector_enabled("enum_dispatch_contracts")
    }

    pub(crate) fn run_public_registry_exposure(&self) -> bool {
        self.detector_enabled("public_registry_exposure")
    }

    pub(crate) fn run_config_key_usage(&self) -> bool {
        self.detector_enabled("config_key_usage")
    }

    pub(crate) fn run_artifact_portability(&self) -> bool {
        self.detector_enabled("artifact_portability")
    }

    pub(crate) fn run_output_capture(&self) -> bool {
        self.detector_enabled("output_capture")
    }

    pub(crate) fn run_command_status_contracts(&self) -> bool {
        self.detector_enabled("command_status_contracts")
    }

    pub(crate) fn run_thin_command_adapter(&self) -> bool {
        self.detector_enabled("thin_command_adapter")
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
}
