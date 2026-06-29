pub mod artifacts;
pub mod audit;
pub mod config;
pub mod drift;
pub mod inventory;
pub mod model;
pub mod mutations;
pub mod portable;
pub mod relationships;
pub mod resolution;
pub mod scope;
pub mod versioning;

pub use artifacts::{cleanup_artifact_report, CleanupArtifactCandidate, CleanupArtifactReport};
pub use audit::{
    ArtifactPortabilityConfig, AuditConfig, CommandStatusContractConfig,
    CommandStatusContractScenario, ConfigKeyUsageConfig, ConfigKeyUsagePattern, ConfigKeyUsageRule,
    ConventionTagGlob, CoreBoundaryLeakConfig, DetectorProfileConfig, DuplicationDetectorConfig,
    KnownSymbolEntry, KnownSymbolHeaderVersionProvider, KnownSymbolKind, KnownSymbolVersionedEntry,
    MutatingResourceAccessConfig, PublicRegistryExposureConfig, RedirectValidationConfig,
    RequestedDetectorRule, RequestedDetectorRuleBody, RequiredRegexScope, SourcePolicyMatchMode,
    SourcePolicyRule, SourcePolicyRuleBody, SourcePolicyTerm, TestWiringConfig, TestWiringPolicy,
    ThinCommandAdapterConfig, ThinCommandAdapterMarkerGroup, VersionSource,
};
pub use config::{
    ArtifactInput, CleanupArtifactDeclaration, CommandScopeConfig, ComponentDeployConfig,
    ComponentScriptsConfig, DependencyStackEdge, GitDeployConfig, GithubConfig, GithubHostConfig,
    ScopeConfig, ScopedExtensionConfig, VersionTarget,
};
pub use inventory::{
    exists, extension_provides_artifact_pattern, inventory, list, list_ids, load,
    reconcile_standalone_registration, write_standalone_component_config,
    write_standalone_registration, ComponentReconcileReport,
};
pub use model::{Component, ComponentLifecycle};
pub use mutations::{delete_safe, merge, rename};
pub use portable::{
    discover_from_portable, infer_portable_component_id, mutate_portable, portable_json,
    try_discover_from_portable, write_portable_config,
};
pub use relationships::{associated_projects, projects_using, rename_component, shared_components};
pub use resolution::{
    local_path_is_relative, normalize_component_local_path,
    normalize_component_local_path_against, resolve, resolve_artifact, resolve_effective,
    resolve_target, resolve_target_from_component, validate_local_path, RegistryLookupPolicy,
    ResolvedTarget, TargetSpec,
};
pub use scope::{resolve_component_scope, EffectiveScope, ScopeCommand};
pub use versioning::{
    normalize_version_pattern, parse_version_targets, validate_version_pattern,
    validate_version_target_conflict,
};

#[cfg(test)]
mod tests;
