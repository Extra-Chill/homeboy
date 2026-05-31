use super::AuditFinding;
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuditExecutionPlan {
    /// Authoritative detector-family execution contract. `run_*` helpers below
    /// derive from this plan so callers do not maintain parallel selector state.
    pub(crate) plan: HomeboyPlan,
}

#[derive(Debug, Clone, Copy)]
struct DetectorFamily {
    id: &'static str,
    findings: &'static [AuditFinding],
    requires_discovery: bool,
}

const DETECTOR_FAMILIES: &[DetectorFamily] = &[
    DetectorFamily {
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
        requires_discovery: true,
    },
    DetectorFamily {
        id: "structural",
        findings: &[
            AuditFinding::GodFile,
            AuditFinding::HighItemCount,
            AuditFinding::DirectorySprawl,
        ],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "duplication",
        findings: &[
            AuditFinding::DuplicateFunction,
            AuditFinding::IntraMethodDuplicate,
            AuditFinding::NearDuplicate,
            AuditFinding::ParallelImplementation,
        ],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "dead_code",
        findings: &[
            AuditFinding::UnusedParameter,
            AuditFinding::IgnoredParameter,
            AuditFinding::DeadCodeMarker,
            AuditFinding::UnreferencedExport,
            AuditFinding::OrphanedInternal,
        ],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "comment_hygiene",
        findings: &[AuditFinding::TodoMarker, AuditFinding::LegacyComment],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "test_coverage",
        findings: &[
            AuditFinding::MissingTestFile,
            AuditFinding::MissingTestMethod,
            AuditFinding::OrphanedTest,
            AuditFinding::VacuousTest,
        ],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "layer_ownership",
        findings: &[AuditFinding::LayerOwnershipViolation],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "test_topology",
        findings: &[
            AuditFinding::InlineTestModule,
            AuditFinding::ScatteredTestFile,
            AuditFinding::VacuousTest,
        ],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "rust_test_wiring",
        findings: &[AuditFinding::UnwiredNestedRustTest],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "docs",
        findings: &[
            AuditFinding::BrokenDocReference,
            AuditFinding::UndocumentedFeature,
            AuditFinding::StaleDocReference,
        ],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "compiler_warnings",
        findings: &[AuditFinding::CompilerWarning],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "wrapper_inference",
        findings: &[AuditFinding::MissingWrapperDeclaration],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "shadow_modules",
        findings: &[AuditFinding::ShadowModule],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "field_patterns",
        findings: &[AuditFinding::RepeatedFieldPattern],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "facade_passthrough",
        findings: &[AuditFinding::FacadePassthrough],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "literal_shapes",
        findings: &[AuditFinding::RepeatedLiteralShape],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "deprecation_age",
        findings: &[AuditFinding::DeprecationAge],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "dead_guard",
        findings: &[AuditFinding::DeadGuard],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "requested_detectors",
        findings: &[
            AuditFinding::JsonLikeExactMatch,
            AuditFinding::ConstantBackedSlugLiteral,
            AuditFinding::OptionScopeDrift,
            AuditFinding::ProxyScopeDrift,
            AuditFinding::ConfigRoundtripAsymmetry,
        ],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "core_boundary_leaks",
        findings: &[AuditFinding::CoreBoundaryLeak],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "mutating_resource_access",
        findings: &[AuditFinding::MutatingResourceAccess],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "redirect_validation",
        findings: &[AuditFinding::RedirectValidation],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "global_env_guard",
        findings: &[AuditFinding::GlobalEnvMutationGuard],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "shared_scaffolding",
        findings: &[AuditFinding::SharedScaffolding],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "parallel_runner_setup",
        findings: &[AuditFinding::ParallelRunnerSetup],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "remote_execution_preflight",
        findings: &[AuditFinding::RemoteExecutionPreflight],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "enum_dispatch_contracts",
        findings: &[AuditFinding::RepeatedEnumDispatchContract],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "aggregate_construction",
        findings: &[AuditFinding::DirectAggregateConstruction],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "public_registry_exposure",
        findings: &[AuditFinding::PublicRegistryExposure],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "config_key_usage",
        findings: &[AuditFinding::WriteOnlyConfigKey],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "artifact_portability",
        findings: &[AuditFinding::NonPortableArtifactPath],
        requires_discovery: false,
    },
    DetectorFamily {
        id: "output_capture",
        findings: &[AuditFinding::UnboundedOutputCapture],
        requires_discovery: true,
    },
    DetectorFamily {
        id: "command_status_contracts",
        findings: &[AuditFinding::CommandStatusContractViolation],
        requires_discovery: false,
    },
];

impl AuditExecutionPlan {
    pub(crate) fn full() -> Self {
        Self::from_enabled_families("full", |family| {
            family.id != "output_capture" && family_enabled(&[], &[], family.findings)
        })
    }

    pub(crate) fn from_filters(only: &[AuditFinding], exclude: &[AuditFinding]) -> Self {
        if only.is_empty() && exclude.is_empty() {
            return Self::full();
        }

        Self::from_enabled_families("filtered", |family| {
            family_enabled(only, exclude, family.findings)
        })
    }

    fn from_enabled_families(mode: &str, is_enabled: impl Fn(&DetectorFamily) -> bool) -> Self {
        let steps: Vec<PlanStep> = DETECTOR_FAMILIES
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

    fn detector_enabled(&self, id: &str) -> bool {
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

    pub(crate) fn run_test_topology(&self) -> bool {
        self.detector_enabled("test_topology")
    }

    pub(crate) fn run_rust_test_wiring(&self) -> bool {
        self.detector_enabled("rust_test_wiring")
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

    pub(crate) fn run_shadow_modules(&self) -> bool {
        self.detector_enabled("shadow_modules")
    }

    pub(crate) fn run_field_patterns(&self) -> bool {
        self.detector_enabled("field_patterns")
    }

    pub(crate) fn run_facade_passthrough(&self) -> bool {
        self.detector_enabled("facade_passthrough")
    }

    pub(crate) fn run_literal_shapes(&self) -> bool {
        self.detector_enabled("literal_shapes")
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

    pub(crate) fn run_mutating_resource_access(&self) -> bool {
        self.detector_enabled("mutating_resource_access")
    }

    pub(crate) fn run_redirect_validation(&self) -> bool {
        self.detector_enabled("redirect_validation")
    }

    pub(crate) fn run_global_env_guard(&self) -> bool {
        self.detector_enabled("global_env_guard")
    }

    pub(crate) fn run_shared_scaffolding(&self) -> bool {
        self.detector_enabled("shared_scaffolding")
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

    pub(crate) fn run_aggregate_construction(&self) -> bool {
        self.detector_enabled("aggregate_construction")
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

    pub(crate) fn requires_discovery(&self) -> bool {
        DETECTOR_FAMILIES
            .iter()
            .any(|family| family.requires_discovery && self.detector_enabled(family.id))
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
