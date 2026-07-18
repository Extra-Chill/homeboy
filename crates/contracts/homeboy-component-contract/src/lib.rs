//! Pure serializable component model + config contract types.
//!
//! The `Component` struct and its `config`-module companions (`VersionTarget`,
//! `GithubConfig`, `ComponentScriptsConfig`, `ScopeConfig`, …) describe the
//! shape of a homeboy component as declared in `homeboy.json`. They are
//! behavior-free data (serde + `homeboy-error` for validation + the
//! `homeboy-audit-contract` / `homeboy-extension-contract` leaf types), so this
//! is a leaf crate other crates can depend on without pulling in core.
//!
//! The extension-driven, filesystem-touching behavior on `Component`
//! (auto-resolving `remote_path` from extension deploy rules) lives in
//! `homeboy-core` as free functions, because it reaches into core's
//! `extension_store` and the filesystem.

pub mod config;
pub mod model;

pub use config::{
    ArtifactInput, CleanupArtifactDeclaration, CommandScopeConfig, ComponentDeployConfig,
    ComponentGithubReleaseConfig, ComponentOverrideConfig, ComponentReleaseConfig,
    ComponentScriptsConfig, DependencyStackEdge, GitDeployConfig, GithubConfig, GithubHostConfig,
    GithubReleaseOwner, PackageCoverageArtifactMatch, PackageCoverageConfig, ScopeConfig,
    ScopedExtensionConfig, VersionTarget,
};
pub use model::{render_remote_path_template, Component, ComponentLifecycle};
