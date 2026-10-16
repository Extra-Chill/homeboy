pub mod artifacts;
// `config` and `model` are the pure serializable component shape declared in
// `homeboy.json`. The extension-driven, fs-touching remote-path resolution over
// `Component` lives in `remote_path` as free functions.
pub mod config;
pub mod drift;
pub mod inventory;
pub mod model;
pub mod mutations;
pub mod portable;
pub mod relationships;
pub mod remote_path;
pub mod resolution;
pub mod scope;
pub mod versioning;

pub use artifacts::{
    cleanup_artifact_report, declared_cleanup_artifact_paths, CleanupArtifactCandidate,
    CleanupArtifactReport,
};
pub use config::{
    ArtifactInput, CleanupArtifactDeclaration, CommandScopeConfig, ComponentDeployConfig,
    ComponentGithubReleaseConfig, ComponentOverrideConfig, ComponentReleaseConfig,
    ComponentScriptsConfig, DependencyStackEdge, DependencyStackEdgeSource,
    DeploymentProviderAttachment, GitDeployConfig, GithubConfig, GithubHostConfig,
    GithubReleaseOwner, PackageArtifact, PackageCoverageArtifactMatch, PackageCoverageConfig,
    ReleaseOwner, ScopeConfig, ScopedExtensionConfig, SubtreeBranchPolicy,
    SubtreePublicationConfig, VersionTarget,
};
pub use inventory::{
    exists, exists_in_root, extension_provides_artifact_pattern,
    extension_provides_artifact_pattern_in_root, inventory, inventory_in_root, list, list_ids,
    list_ids_in_root, list_in_root, load, load_in_root, reconcile_standalone_registration,
    registered, registered_base, registered_by_id, registered_by_id_in_root,
    registered_by_local_path, registered_by_local_path_in_root, registered_in_root,
    write_standalone_component_config, write_standalone_registration, ComponentReconcileReport,
};
pub use model::{Component, ComponentLifecycle, ComponentManagedExecution};
pub use mutations::{delete_safe, merge, rename};
pub use portable::{
    discover_from_portable, infer_portable_component_id, mutate_portable, portable_json,
    try_discover_from_portable, write_portable_config,
};
pub use relationships::{associated_projects, projects_using, rename_component, shared_components};
pub use remote_path::{
    auto_resolve_remote_path, auto_resolve_remote_path_in_root, resolve_remote_path,
    resolve_remote_path_in_root, try_auto_resolve_remote_path,
    try_auto_resolve_remote_path_in_root,
};
pub use resolution::{
    local_path_is_relative, normalize_component_local_path, normalize_component_local_path_against,
    resolve, resolve_artifact, resolve_artifact_in_root, resolve_effective,
    resolve_effective_in_root, resolve_in_root, resolve_registered_primary_identity,
    resolve_registered_primary_path, resolve_target, resolve_target_from_component,
    validate_local_path, RegisteredPathCandidates, RegisteredPrimaryPathResolution,
    RegistryLookupPolicy, ResolvedTarget, TargetSpec,
};
pub use scope::{resolve_component_scope, EffectiveScope, ScopeCommand};
pub use versioning::{
    normalize_version_pattern, parse_capability_extensions, parse_version_targets,
    validate_version_pattern, validate_version_target_conflict,
};

#[cfg(test)]
mod tests;
