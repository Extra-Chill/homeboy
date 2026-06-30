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
        schema_id: crate::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA,
        name: "secret-env-plan",
        title: "Secret environment plan",
        owner: "homeboy-core",
        summary: "Describes required secret environment variables while keeping values out of serialized plans.",
        rust_type: "homeboy::core::secret_env_plan::SecretEnvPlan",
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
        schema_id: crate::command_contract::RUN_LOCATION_INDEX_SCHEMA,
        name: "run-location-index",
        title: "Run location index",
        owner: "homeboy-core",
        summary: "Maps a runner-resident run to its cwd, artifact directory, and artifact manifest reference.",
        rust_type: "homeboy::command_contract::RunLocationIndex",
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

    #[test]
    fn registry_covers_first_slice_contracts() {
        let names = CONTRACT_REGISTRY
            .iter()
            .map(|entry| entry.name)
            .collect::<std::collections::BTreeSet<_>>();

        for name in [
            "lifecycle-contract",
            "lifecycle-result",
            "lifecycle-snapshot-ref",
            "run-lifecycle-status",
            "artifact-manifest",
            "secret-env-plan",
            "fuzz-workload",
            "resource-cleanup-intent",
            "run-location-index",
            "extension-materialization-source",
            "resolved-agent-runtime-execution-contract",
            "reviewer-facing-artifact-ref",
        ] {
            assert!(names.contains(name), "missing {name}");
        }
    }
}
