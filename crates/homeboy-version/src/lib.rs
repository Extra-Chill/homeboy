//! Shared version, changelog, and release-tag-naming primitives.
//!
//! `homeboy-deploy` and `homeboy-release` both need to read a component's
//! version, parse and rewrite version patterns, inspect changelog sections, and
//! resolve the tag namespace a component releases under. Before this crate
//! existed those primitives lived under `release/`, and `deploy/` reached back
//! into them — a genuine dependency cycle, and the reason the two subsystems
//! could not simply be split into their own crates (#11144).
//!
//! Everything here depends only on `homeboy-core` (plus the extension contract for
//! extension-declared hook events), so it sits strictly below both
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

/// Resolve the version a git ref names, when that ref is this component's
/// release tag.
///
/// The exact inverse of [`component_tag_name`], and the shared answer to "is
/// this ref a release tag?". Returns `None` for branches, raw SHAs, and tags
/// outside this component's release namespace.
///
/// Deploy needs this to decide whether an exact `--ref` addresses a release
/// whose canonical asset should be reused rather than rebuilt locally (#12215).
/// It lives here, next to the tag naming contract it inverts, so the two cannot
/// drift apart — and, like the rest of this crate, it stays ecosystem-agnostic.
pub fn component_release_tag_version(
    component: &Component,
    git_ref: &str,
) -> homeboy_core::Result<Option<String>> {
    let scope = scope::ReleaseScope::resolve(component, &component.id)?;
    Ok(release_tag_version_for_prefix(git_ref, scope.tag_prefix()))
}

/// The version to treat as this component's "local" configured version when it
/// has no local checkout but declares a GitHub `remote_url` — the version
/// implied by that repository's latest GitHub Release tag, which is the only
/// version authority available without reading a source tree (#14782).
///
/// Returns `Ok(None)` when the ordinary [`version::get_component_version`]
/// path applies instead (the component has a checkout), when it has no
/// GitHub `remote_url`, or when the repository has no release yet. Callers
/// use this as a fallback after `version::get_component_version` returns
/// `None`, not as a replacement for it — a component with a checkout keeps
/// reading its actual configured version, not "whatever GitHub last shipped".
pub fn checkout_less_release_version(
    component: &Component,
) -> homeboy_core::Result<Option<String>> {
    if component.has_local_checkout() {
        return Ok(None);
    }
    let Some(remote_url) = component.remote_url.as_deref() else {
        return Ok(None);
    };
    let Some(github) = homeboy_core::git::release_download::parse_github_url(remote_url) else {
        return Ok(None);
    };
    let Some(tag) = homeboy_core::git::release_download::latest_release_tag_for_repo(
        &github,
        &component.github,
    )?
    else {
        return Ok(None);
    };

    component_release_tag_version(component, &tag)
}

/// Strip the namespace a component tags under and keep what remains only when
/// it is a real version.
///
/// The version check is what separates a release tag from a same-shaped branch:
/// `v2` and `release/v1` are not releases, `v1.2.3` is.
fn release_tag_version_for_prefix(git_ref: &str, tag_prefix: Option<&str>) -> Option<String> {
    let version = match tag_prefix {
        Some(prefix) => git_ref.strip_prefix(&format!("{prefix}-v"))?,
        None => git_ref.strip_prefix('v')?,
    };

    semver::Version::parse(version)
        .ok()
        .map(|_| version.to_string())
}

#[cfg(test)]
mod release_tag_version_tests {
    use super::release_tag_version_for_prefix;

    #[test]
    fn a_root_component_release_tag_resolves_its_version() {
        assert_eq!(
            release_tag_version_for_prefix("v1.2.3", None),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn a_scoped_component_release_tag_resolves_its_version() {
        assert_eq!(
            release_tag_version_for_prefix(
                "data-machine-events-v0.55.1",
                Some("data-machine-events")
            ),
            Some("0.55.1".to_string())
        );
    }

    #[test]
    fn a_tag_outside_the_components_namespace_is_not_a_release_tag() {
        // A scoped component does not release under bare `vX.Y.Z`, and must not
        // claim another component's namespace.
        assert_eq!(
            release_tag_version_for_prefix("v0.55.1", Some("events")),
            None
        );
        assert_eq!(
            release_tag_version_for_prefix("other-component-v0.55.1", Some("events")),
            None
        );
    }

    #[test]
    fn branches_and_raw_shas_are_not_release_tags() {
        assert_eq!(release_tag_version_for_prefix("main", None), None);
        assert_eq!(release_tag_version_for_prefix("release/v1.2.3", None), None);
        assert_eq!(
            release_tag_version_for_prefix("9b780f558f0e4a1b2c3d4e5f60718293a4b5c6d7", None),
            None
        );
        // A `v`-prefixed branch name is the case that makes the version parse
        // load-bearing rather than decorative.
        assert_eq!(release_tag_version_for_prefix("vnext", None), None);
        assert_eq!(release_tag_version_for_prefix("v2", None), None);
    }

    #[test]
    fn prerelease_and_build_metadata_tags_are_release_tags() {
        assert_eq!(
            release_tag_version_for_prefix("v1.2.3-rc.1", None),
            Some("1.2.3-rc.1".to_string())
        );
    }
}

#[cfg(test)]
mod checkout_less_release_version_tests {
    use super::checkout_less_release_version;
    use homeboy_core::component::Component;

    #[cfg(unix)]
    fn write_fake_binary(path: &std::path::Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script).expect("write fake binary");
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod fake binary");
    }

    #[test]
    fn is_none_when_a_local_checkout_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            remote_url: Some(
                "https://github.com/Extra-Chill-Test/checkout-exists-fixture".to_string(),
            ),
            ..Component::default()
        };

        // A component with a real checkout uses `version::get_component_version`;
        // this fallback only ever applies when that path has nothing to read.
        assert_eq!(
            checkout_less_release_version(&component).expect("resolves"),
            None
        );
    }

    #[test]
    fn is_none_without_a_remote_url() {
        let component = Component {
            id: "fixture".to_string(),
            local_path: "/tmp/homeboy-14782-no-remote-url".to_string(),
            ..Component::default()
        };

        assert_eq!(
            checkout_less_release_version(&component).expect("resolves"),
            None
        );
    }

    #[test]
    fn is_none_for_a_non_github_remote_url() {
        let component = Component {
            id: "fixture".to_string(),
            local_path: "/tmp/homeboy-14782-non-github-remote".to_string(),
            remote_url: Some("https://gitlab.com/example/fixture.git".to_string()),
            ..Component::default()
        };

        assert_eq!(
            checkout_less_release_version(&component).expect("resolves"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_version_implied_by_the_latest_release_tag() {
        let _lock = homeboy_core::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_binary(
            &bin_dir.path().join("gh"),
            "#!/bin/sh\necho fake-test-token\nexit 0\n",
        );
        write_fake_binary(
            &bin_dir.path().join("curl"),
            "#!/bin/sh\ncat > /dev/null 2>&1\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  \
             *releases/latest*) printf '%s' '{\"tag_name\":\"v1.4.2\"}'; printf '\\n200' ;;\n  \
             *) printf '\\n404' ;;\nesac\n",
        );
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = bin_dir.path().as_os_str().to_os_string();
        new_path.push(":");
        new_path.push(existing);
        let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", new_path);

        let component = Component {
            id: "fixture".to_string(),
            local_path: "/tmp/homeboy-14782-checkout-less-version".to_string(),
            remote_url: Some(
                "https://github.com/Extra-Chill-Test/checkout-less-version-fixture".to_string(),
            ),
            ..Component::default()
        };

        assert_eq!(
            checkout_less_release_version(&component).expect("resolves"),
            Some("1.4.2".to_string())
        );
    }
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
