use std::collections::BTreeSet;

use homeboy_core::component::{Component, VersionTarget};
use homeboy_core::execution::ChangeArtifactProvenance;

use crate::release::changelog::generated_file_mutation_is_authorized_for;

use super::{parse_versions, resolve_target_pattern};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMutation {
    pub file: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOwnedMutationViolation {
    pub file: String,
    pub message: String,
}

/// Reject version-field and release-derived lockfile mutations that were not
/// produced by the release version step. Target matching remains entirely
/// configuration-driven: a target's configured regex identifies its version
/// field, regardless of whether it is a PHP header, JSON manifest, or text file.
pub fn detect_manual_release_owned_mutations(
    component: &Component,
    mutations: &[VersionMutation],
    provenance: Option<&ChangeArtifactProvenance>,
) -> Vec<ReleaseOwnedMutationViolation> {
    if generated_file_mutation_is_authorized_for(provenance, "version") {
        return Vec::new();
    }

    let targets = component.version_targets.as_deref().unwrap_or_default();
    let derived_lockfiles = derived_lockfiles(targets);
    let declared_lockfiles =
        homeboy_core::component::drift::extension_declared_lockfile_paths(component)
            .into_iter()
            .collect::<BTreeSet<_>>();
    mutations
        .iter()
        .filter_map(|mutation| {
            let file = normalize_path(&mutation.file)?;
            let target = targets
                .iter()
                .find(|target| normalize_path(&target.file).as_deref() == Some(&file));
            let manually_maintained = component
                .release
                .manual_version_targets
                .iter()
                .filter_map(|path| normalize_path(path))
                .any(|path| path == file);

            if derived_lockfiles.contains(&file) || declared_lockfiles.contains(&file) {
                let lockfile_manually_maintained = targets.iter().any(|target| {
                    component
                        .release
                        .manual_version_targets
                        .iter()
                        .any(|configured| {
                            normalize_path(configured) == normalize_path(&target.file)
                        })
                        && crate::release::planning_worktree::derived_release_lockfiles(
                            &target.file,
                        )
                        .iter()
                        .filter_map(|path| normalize_path(path))
                        .any(|path| path == file)
                });
                let declared_lockfile_manually_maintained =
                    manual_release_lockfile_is_allowed(component, &file);
                return (!lockfile_manually_maintained && !declared_lockfile_manually_maintained)
                    .then(|| {
                        violation(
                            &file,
                            if declared_lockfiles.contains(&file) {
                                "extension-declared release lockfile"
                            } else {
                                "release-derived lockfile"
                            },
                        )
                    });
            }

            let target = target?;
            let pattern = resolve_target_pattern(target).ok()?;
            let before = parse_versions(&mutation.before, &pattern)?;
            let after = parse_versions(&mutation.after, &pattern)?;
            (before != after && !manually_maintained)
                .then(|| violation(&file, "configured version field"))
        })
        .collect()
}

fn manual_release_lockfile_is_allowed(component: &Component, file: &str) -> bool {
    component
        .release
        .manual_release_lockfiles
        .iter()
        .filter_map(|path| normalize_path(path))
        .any(|path| path == file)
}

pub fn derived_lockfiles(targets: &[VersionTarget]) -> BTreeSet<String> {
    targets
        .iter()
        .flat_map(|target| {
            crate::release::planning_worktree::derived_release_lockfiles(&target.file)
        })
        .filter_map(|path| normalize_path(&path))
        .collect()
}

pub fn release_owned_lockfiles(component: &Component) -> BTreeSet<String> {
    let mut lockfiles = derived_lockfiles(component.version_targets.as_deref().unwrap_or_default());
    lockfiles.extend(homeboy_core::component::drift::extension_declared_lockfile_paths(component));
    lockfiles
}

fn normalize_path(path: &str) -> Option<String> {
    let path = std::path::Path::new(path);
    if path.is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn violation(file: &str, kind: &str) -> ReleaseOwnedMutationViolation {
    ReleaseOwnedMutationViolation {
        file: file.to_string(),
        message: format!(
            "{file} changed a {kind}. Release-owned mutations require durable Homeboy release provenance from the `version` step; configure `release.manual_version_targets` only for intentional manual maintenance."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(file: &str, pattern: &str) -> Component {
        Component {
            version_targets: Some(vec![VersionTarget {
                file: file.to_string(),
                pattern: Some(pattern.to_string()),
                artifact_path: None,
            }]),
            ..Default::default()
        }
    }

    fn mutation(file: &str, before: &str, after: &str) -> VersionMutation {
        VersionMutation {
            file: file.to_string(),
            before: before.to_string(),
            after: after.to_string(),
        }
    }

    #[test]
    fn rejects_direct_php_and_package_version_bumps() {
        let php = component("plugin.php", r"Version:\s*([0-9.]+)");
        let package = component("package.json", r#""version"\s*:\s*"([0-9.]+)""#);

        assert_eq!(
            detect_manual_release_owned_mutations(
                &php,
                &[mutation(
                    "plugin.php",
                    "/*\n * Version: 1.0.0\n */",
                    "/*\n * Version: 1.0.1\n */"
                )],
                None
            )
            .len(),
            1
        );
        assert_eq!(
            detect_manual_release_owned_mutations(
                &package,
                &[mutation(
                    "package.json",
                    r#"{"version":"1.0.0"}"#,
                    r#"{"version":"1.0.1"}"#
                )],
                None
            )
            .len(),
            1
        );
    }

    #[test]
    fn ignores_manifest_edits_when_the_configured_version_is_unchanged() {
        let component = component("package.json", r#""version"\s*:\s*"([0-9.]+)""#);
        let violations = detect_manual_release_owned_mutations(
            &component,
            &[mutation(
                "package.json",
                r#"{"version":"1.0.0","description":"before"}"#,
                r#"{"version":"1.0.0","description":"after"}"#,
            )],
            None,
        );
        assert!(violations.is_empty());
    }

    #[test]
    fn accepts_only_durable_version_step_provenance() {
        let component = component("VERSION", r"([0-9.]+)");
        let mutation = mutation("VERSION", "1.0.0", "1.0.1");
        let authorized = ChangeArtifactProvenance {
            source: "release".to_string(),
            run_id: Some("release.component".to_string()),
            step_id: Some("version".to_string()),
            command: None,
            captured_at: None,
        };
        let malformed = ChangeArtifactProvenance {
            source: "release".to_string(),
            run_id: Some(" ".to_string()),
            step_id: Some("version".to_string()),
            command: None,
            captured_at: None,
        };

        assert!(detect_manual_release_owned_mutations(
            &component,
            &[mutation.clone()],
            Some(&authorized)
        )
        .is_empty());
        assert_eq!(
            detect_manual_release_owned_mutations(&component, &[mutation], Some(&malformed)).len(),
            1
        );
    }

    #[test]
    fn protects_derived_lockfiles_and_allows_a_target_scoped_opt_out() {
        let mut component = component("Cargo.toml", r#"version\s*=\s*"([0-9.]+)""#);
        assert_eq!(
            detect_manual_release_owned_mutations(
                &component,
                &[mutation("Cargo.lock", "old", "new")],
                None
            )
            .len(),
            1
        );

        component
            .release
            .manual_version_targets
            .push("Cargo.toml".to_string());
        assert!(detect_manual_release_owned_mutations(
            &component,
            &[
                mutation("Cargo.toml", r#"version = "1.0.0""#, r#"version = "1.0.1""#),
                mutation("Cargo.lock", "old", "new")
            ],
            None
        )
        .is_empty());
    }

    #[test]
    fn declared_lockfile_opt_out_is_exact_path_scoped() {
        let mut component = Component::default();
        component
            .release
            .manual_release_lockfiles
            .push("./nested/package-lock.json".to_string());

        assert!(manual_release_lockfile_is_allowed(
            &component,
            "nested/package-lock.json"
        ));
        assert!(!manual_release_lockfile_is_allowed(
            &component,
            "package-lock.json"
        ));
    }
}
