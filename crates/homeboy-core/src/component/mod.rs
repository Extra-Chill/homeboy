pub mod artifacts;
pub mod audit_provider;
// The audit config schema was extracted to the homeboy-audit-contract crate
// (#8425) so it no longer couples component <-> code_audit through the core
// module tree. Re-exported as `component::audit` so existing
// `crate::component::audit::*` paths keep resolving.
pub use homeboy_audit_contract as audit;
// The pure component model + config data types were extracted to the
// homeboy-component-contract leaf crate. Re-exported as `component::config` and
// `component::model` so existing `crate::component::{config,model}::*` paths
// keep resolving. The extension-driven, fs-touching remote-path resolution that
// used to live on `Component` stays in core as free functions in
// `remote_path`.
pub use homeboy_component_contract::config;
pub use homeboy_component_contract::model;
pub mod drift;
pub mod inventory;
pub mod mutations;
pub mod portable;
pub mod relationships;
pub mod remote_path;
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
    ComponentGithubReleaseConfig, ComponentOverrideConfig, ComponentReleaseConfig,
    ComponentScriptsConfig, DependencyStackEdge, GitDeployConfig, GithubConfig, GithubHostConfig,
    GithubReleaseOwner, PackageCoverageArtifactMatch, PackageCoverageConfig, ScopeConfig,
    ScopedExtensionConfig, VersionTarget,
};
pub use inventory::{
    exists, extension_provides_artifact_pattern, inventory, list, list_ids, load,
    reconcile_standalone_registration, registered, write_standalone_component_config,
    write_standalone_registration, ComponentReconcileReport,
};
pub use model::{Component, ComponentLifecycle};
pub use mutations::{delete_safe, merge, rename};
pub use portable::{
    discover_from_portable, infer_portable_component_id, mutate_portable, portable_json,
    try_discover_from_portable, write_portable_config,
};
pub use relationships::{associated_projects, projects_using, rename_component, shared_components};
pub use remote_path::{auto_resolve_remote_path, resolve_remote_path};
pub use resolution::{
    local_path_is_relative, normalize_component_local_path, normalize_component_local_path_against,
    resolve, resolve_artifact, resolve_effective, resolve_target, resolve_target_from_component,
    validate_local_path, RegistryLookupPolicy, ResolvedTarget, TargetSpec,
};
pub use scope::{resolve_component_scope, EffectiveScope, ScopeCommand};
pub use versioning::{
    normalize_version_pattern, parse_version_targets, validate_version_pattern,
    validate_version_target_conflict,
};

#[cfg(test)]
mod tests;
