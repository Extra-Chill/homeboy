//! Core-owned data contract registry.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ContractRegistryEntry {
    pub schema_id: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub owner: &'static str,
    pub summary: &'static str,
    pub rust_type: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ContractRegistrySummary {
    pub schema_id: &'static str,
    pub name: &'static str,
    pub title: &'static str,
}

impl ContractRegistryEntry {
    pub const fn summary(&self) -> ContractRegistrySummary {
        ContractRegistrySummary {
            schema_id: self.schema_id,
            name: self.name,
            title: self.title,
        }
    }

    pub fn matches(&self, value: &str) -> bool {
        self.schema_id == value || self.name == value
    }
}

pub const CONTRACT_REGISTRY: &[ContractRegistryEntry] = &[
    ContractRegistryEntry {
        schema_id: crate::core::release_set::RELEASE_SET_SCHEMA,
        name: "release-set",
        title: "Release set manifest",
        owner: "homeboy-core",
        summary: "Caller-supplied, product-agnostic required or optional component membership and immutable source references.",
        rust_type: "homeboy::core::release_set::ReleaseSetManifest",
    },
    ContractRegistryEntry {
        schema_id: crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA,
        name: "lifecycle-contract",
        title: "Loop lifecycle contract",
        owner: "homeboy-core",
        summary: "Declares generic mutable-workload lifecycle phases such as prepare, seed, snapshot, reset, rollback, and teardown.",
        rust_type: "homeboy::core::lifecycle::LifecycleContract",
    },
    ContractRegistryEntry {
        schema_id: crate::core::lifecycle::LIFECYCLE_RESULT_SCHEMA,
        name: "lifecycle-result",
        title: "Loop lifecycle result",
        owner: "homeboy-core",
        summary: "Records lifecycle phase outcomes and optional snapshot references for generic workload execution.",
        rust_type: "homeboy::core::lifecycle::LifecycleResultMetadata",
    },
    ContractRegistryEntry {
        schema_id: crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA,
        name: "lifecycle-snapshot-ref",
        title: "Loop lifecycle snapshot reference",
        owner: "homeboy-core",
        summary: "Identifies a snapshot captured during a lifecycle phase without coupling to a product runtime.",
        rust_type: "homeboy::core::lifecycle::LifecycleSnapshotRef",
    },
    ContractRegistryEntry {
        schema_id: crate::core::run_lifecycle_status::RUN_LIFECYCLE_STATUS_SCHEMA,
        name: "run-lifecycle-status",
        title: "Run lifecycle status",
        owner: "homeboy-core",
        summary: "Canonical generic status vocabulary for persisted or runner-backed runs.",
        rust_type: "homeboy::core::run_lifecycle_status::RunLifecycleStatus",
    },
    ContractRegistryEntry {
        schema_id: crate::core::artifacts::ARTIFACT_MANIFEST_SCHEMA,
        name: "artifact-manifest",
        title: "Artifact manifest",
        owner: "homeboy-core",
        summary: "Indexes produced artifacts, provenance, redaction state, and reviewer-viewer links.",
        rust_type: "homeboy::core::artifact_manifest::ArtifactManifest",
    },
    ContractRegistryEntry {
        schema_id: crate::core::artifacts::RUNTIME_AGENT_ARTIFACT_PATHS_SCHEMA,
        name: "artifact-paths",
        title: "Runtime artifact paths",
        owner: "homeboy-core",
        summary: "Publishes stable runtime artifact path and filename constants for downstream consumers.",
        rust_type: "homeboy::command_contract::ArtifactPathsConstants",
    },
    ContractRegistryEntry {
        schema_id: crate::core::artifacts::ARTIFACT_POSTPROCESS_SCHEMA,
        name: "artifact-postprocess",
        title: "Artifact postprocess plan",
        owner: "homeboy-core",
        summary: "Declares generic helper-driven postprocessing actions for produced artifacts without product-specific semantics.",
        rust_type: "homeboy::core::artifact_postprocess::ArtifactPostprocessPlan",
    },
    ContractRegistryEntry {
        schema_id: crate::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA,
        name: "secret-env-plan",
        title: "Secret environment plan",
        owner: "homeboy-core",
        summary: "Describes required secret environment variables while keeping values out of serialized plans.",
        rust_type: "homeboy::core::secret_env_plan::SecretEnvPlan",
    },
    ContractRegistryEntry {
        schema_id: crate::core::env_materialization_plan::ENV_MATERIALIZATION_PLAN_SCHEMA,
        name: "env-materialization-plan",
        title: "Environment materialization plan",
        owner: "homeboy-core",
        summary: "Describes public env literals, secret refs, source env refs, inherited allowlists, and handoff metadata without serialized secret values.",
        rust_type: "homeboy::core::env_materialization_plan::EnvMaterializationPlan",
    },
    ContractRegistryEntry {
        schema_id: crate::core::secret_env_plan::SECRET_ENV_MATERIALIZED_HANDOFF_SCHEMA,
        name: "secret-env-materialized-handoff",
        title: "Secret environment materialized handoff",
        owner: "homeboy-core",
        summary: "Projects materialized secret environment keys, source labels, missing names, and diagnostics without secret values.",
        rust_type: "homeboy::core::secret_env_plan::SecretEnvMaterializedHandoff",
    },
    ContractRegistryEntry {
        schema_id: crate::core::fuzz::FUZZ_WORKLOAD_SCHEMA,
        name: "fuzz-workload",
        title: "Fuzz workload",
        owner: "homeboy-core",
        summary: "Generic fuzz workload declaration consumed by Homeboy fuzz orchestration.",
        rust_type: "homeboy::core::fuzz::FuzzWorkload",
    },
    ContractRegistryEntry {
        schema_id: crate::core::resource_cleanup_intent::RESOURCE_CLEANUP_INTENT_SCHEMA,
        name: "resource-cleanup-intent",
        title: "Resource cleanup intent",
        owner: "homeboy-core",
        summary: "Declares dry-run versus apply cleanup intent and ownership metadata for removable resources.",
        rust_type: "homeboy::core::resource_cleanup_intent::ResourceCleanupIntentContract",
    },
    ContractRegistryEntry {
        schema_id: crate::core::resource_lifecycle_index::RESOURCE_LIFECYCLE_INDEX_SCHEMA,
        name: "resource-lifecycle-index",
        title: "Resource lifecycle index",
        owner: "homeboy-core",
        summary: "Indexes run-owned resources, retention policy, cleanup intent, and lifecycle status without runtime-specific semantics.",
        rust_type: "homeboy::core::resource_lifecycle_index::ResourceLifecycleIndex",
    },
    ContractRegistryEntry {
        schema_id: crate::core::host_mutation_lifecycle::HOST_MUTATION_LIFECYCLE_SCHEMA,
        name: "host-mutation-lifecycle",
        title: "Host mutation lifecycle",
        owner: "homeboy-core",
        summary: "Declares reversible host mutations such as symlinks, temp dirs, file backups, and package manifest rewrites without product-specific semantics.",
        rust_type: "homeboy::core::host_mutation_lifecycle::HostMutationLifecycle",
    },
    ContractRegistryEntry {
        schema_id: crate::command_contract::RUN_LOCATION_INDEX_SCHEMA,
        name: "run-location-index",
        title: "Run location index",
        owner: "homeboy-core",
        summary: "Maps a runner-resident run to its cwd, artifact directory, and artifact manifest reference.",
        rust_type: "homeboy::command_contract::RunLocationIndex",
    },
    ContractRegistryEntry {
        schema_id: crate::command_contract::RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
        name: crate::command_contract::RUNNER_ARTIFACT_MANIFEST_REF_NAME,
        title: "Runner artifact manifest reference",
        owner: "homeboy-core",
        summary: "Points to a runner-resident artifact manifest and declares the manifest schema it contains.",
        rust_type: "homeboy::command_contract::RunnerHandoffArtifactManifestRef",
    },
    ContractRegistryEntry {
        schema_id: crate::core::extension::EXTENSION_MATERIALIZATION_SOURCE_SCHEMA,
        name: "extension-materialization-source",
        title: "Extension materialization source",
        owner: "homeboy-core",
        summary: "Records where an installed extension was materialized from in a generic source contract.",
        rust_type: "homeboy::core::extension::ExtensionMaterializationSourceContract",
    },
    ContractRegistryEntry {
        schema_id: crate::core::agent_task_provider::RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA,
        name: "resolved-agent-runtime-execution-contract",
        title: "Resolved agent runtime execution contract",
        owner: "homeboy-core",
        summary: "Captures the selected agent runtime provider, materialization, secrets, readiness, and capabilities.",
        rust_type: "homeboy::core::agent_task_provider::ResolvedAgentRuntimeExecutionContract",
    },
    ContractRegistryEntry {
        schema_id: crate::core::artifact_ref::ARTIFACT_REF_SCHEMA,
        name: "reviewer-facing-artifact-ref",
        title: "Reviewer-facing artifact reference",
        owner: "homeboy-core",
        summary: "Stable artifact reference vocabulary suitable for reviewer-facing evidence links.",
        rust_type: "homeboy::core::artifact_ref::ArtifactReference",
    },
    ContractRegistryEntry {
        schema_id: crate::core::run_outcome_envelope::RUN_OUTCOME_ENVELOPE_SCHEMA,
        name: "run-outcome-envelope",
        title: "Run outcome envelope",
        owner: "homeboy-core",
        summary: "Canonical terminal run outcome envelope for runner and Lab executions, including status, exit code, artifact/evidence references, handoffs, and result payloads.",
        rust_type: "homeboy::core::run_outcome_envelope::RunOutcomeEnvelope",
    },
    ContractRegistryEntry {
        schema_id: crate::core::runner_execution_envelope::RUNNER_EXECUTION_ENVELOPE_SCHEMA,
        name: "runner-execution-envelope",
        title: "Runner execution envelope",
        owner: "homeboy-core",
        summary: "Generic planned runner execution request with source, secret, artifact, mutation, publication, and result-ref policy.",
        rust_type: "homeboy::core::runner_execution_envelope::RunnerExecutionEnvelope",
    },
    ContractRegistryEntry {
        schema_id: crate::core::runner_execution_envelope::RUNNER_EXECUTION_RECORD_SCHEMA,
        name: "runner-execution-record",
        title: "Runner execution record",
        owner: "homeboy-core",
        summary: "Durable runner execution record for orchestration identity, materialized paths, provenance, and follow-up actions; terminal command output should project through run-outcome-envelope.",
        rust_type: "homeboy::core::runner_execution_envelope::RunnerExecutionRecord",
    },
    ContractRegistryEntry {
        schema_id: crate::core::runner_execution_envelope::PATH_MATERIALIZATION_PLAN_SCHEMA,
        name: "path-materialization-plan",
        title: "Path materialization plan",
        owner: "homeboy-core",
        summary: "Generic runner-side path materialization entries for source snapshots and required remote paths.",
        rust_type: "homeboy::core::runner_execution_envelope::PathMaterializationPlan",
    },
    ContractRegistryEntry {
        schema_id: crate::core::runner_execution_envelope::ORCHESTRATION_TARGET_PROVENANCE_SCHEMA,
        name: "orchestration-target-provenance",
        title: "Orchestration target provenance",
        owner: "homeboy-core",
        summary: "Captures the controller, runner daemon, runner command, source snapshot, and extension provenance for a selected runner target.",
        rust_type: "homeboy::core::runner_execution_envelope::OrchestrationTargetProvenance",
    },
];

pub fn registered_contracts() -> &'static [ContractRegistryEntry] {
    CONTRACT_REGISTRY
}

pub fn registered_contract(value: &str) -> Option<&'static ContractRegistryEntry> {
    CONTRACT_REGISTRY.iter().find(|entry| entry.matches(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_by_schema_id_or_name() {
        let by_schema = registered_contract(crate::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA)
            .expect("schema lookup");
        let by_name = registered_contract("secret-env-plan").expect("name lookup");

        assert_eq!(by_schema, by_name);
        assert_eq!(by_name.title, "Secret environment plan");
    }
}
