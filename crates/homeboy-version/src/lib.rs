//! Shared version, changelog, and release-tag-naming primitives.
//!
//! `homeboy-deploy` and `homeboy-release` both need to read a component's
//! version, parse and rewrite version patterns, inspect changelog sections, and
//! resolve the tag namespace a component releases under. Before this crate
//! existed those primitives lived under `release/`, and `deploy/` reached back
//! into them — a genuine dependency cycle, and the reason the two subsystems
//! could not simply be split into their own crates (#11144).
//!
//! Everything here depends only on `homeboy-core` (plus `homeboy-extension` for
//! extension-declared version patterns), so it sits strictly below both
//! subsystems and makes `homeboy-deploy` -> `homeboy-release` a one-way edge.
//!
//! Deliberately *not* here: the release-owned mutation guard
//! (`homeboy-release`'s `version_guard`), which needs release planning
//! internals, and anything that produces `homeboy-deploy` types.

pub mod changelog;
pub mod scope;
pub mod version;

use homeboy_core::component::Component;

/// Return the release tag name this component uses for a version.
///
/// This is the shared tag naming contract for release, deploy, status, and
/// changes. Components scoped below a repository root use component-prefixed
/// tags; root components use plain `vX.Y.Z` tags.
pub fn component_tag_name(component: &Component, version: &str) -> homeboy_core::Result<String> {
    let version = version.trim_start_matches('v');
    let scope = scope::ReleaseScope::resolve(component, &component.id)?;
    Ok(scope.tag_name(version))
}

/// Return the tag prefix this component uses, when it has a component-scoped
/// release namespace.
pub fn component_tag_prefix(component: &Component) -> homeboy_core::Result<Option<String>> {
    let scope = scope::ReleaseScope::resolve(component, &component.id)?;
    Ok(scope.tag_prefix().map(str::to_string))
}

/// Resolve the latest release tag for this component using the same namespace
/// that release uses when creating tags.
pub fn latest_component_tag(component: &Component) -> homeboy_core::Result<Option<String>> {
    let scope = scope::ReleaseScope::resolve(component, &component.id)?;
    scope.latest_tag()
}

#[cfg(test)]
mod tests {
    /// The shared version reader must not branch on ecosystem-specific terms —
    /// it is consumed by every extension's release and deploy path.
    ///
    /// This assertion moved here with `version.rs` when it was lifted out of
    /// `homeboy-release` (#11144); it previously lived in that crate's
    /// `pipeline` facade alongside the same check for `executor.rs`.
    #[test]
    fn version_core_stays_ecosystem_agnostic() {
        // `component_version.rs` carries the single version-read spine that
        // used to be three copies inside `version.rs` (#11144), so it is held
        // to the same contract.
        let sources = [
            ("version.rs", include_str!("version.rs")),
            (
                "version/component_version.rs",
                include_str!("version/component_version.rs"),
            ),
        ];
        for (file, source) in sources {
            let runtime_source = source.split("#[cfg(test)]").next().unwrap_or(source);
            for term in ["Cargo", "cargo", "Rust", "rust"] {
                assert!(
                    !runtime_source.contains(term),
                    "version core ({file}) must not branch on ecosystem-specific term {term:?}"
                );
            }
        }
    }
}
