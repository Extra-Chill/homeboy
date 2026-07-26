//! Resolve extension-owned artifact cleanup declarations against a worktree.
//!
//! Extensions declare which install/build trees they own and how to rehydrate
//! them. This module turns those declarations into concrete worktree-relative
//! artifact paths. It deliberately learns nothing about any ecosystem: an
//! install scope is "a directory carrying the manifest files the declaration
//! names", and nested resolution is a bounded walk that never descends into a
//! declared artifact tree.
//!
//! Removal policy (dry-run/apply, containment, age and liveness gates, Git
//! safety, byte accounting) stays with the caller in `cleanup`.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use homeboy_extension_contract::manifest_artifact_cleanup::{
    ArtifactCleanupDeclaration, ArtifactCleanupScope,
};

use super::ArtifactDeclaration;

/// Upper bound on directories inspected while resolving nested install scopes.
/// Nested discovery is a convenience, not a full-tree indexer; a pathological
/// tree must not turn cleanup inventory into an unbounded crawl.
const MAX_SCOPE_SCAN_DIRECTORIES: usize = 20_000;

/// Resolve every installed extension's declarations against one worktree.
///
/// Extension load failures are non-fatal: a broken or unreadable manifest must
/// not take down cleanup inventory for every other declaration.
pub(super) fn extension_artifact_declarations(worktree: &Path) -> Vec<ArtifactDeclaration> {
    let Ok(extensions) = crate::extension_store::load_all_extensions() else {
        return Vec::new();
    };

    let owned: Vec<(String, Vec<ArtifactCleanupDeclaration>)> = extensions
        .into_iter()
        .filter(|extension| !extension.artifact_cleanup.is_empty())
        .map(|extension| {
            (
                extension.id.clone(),
                extension.artifact_cleanup.declarations,
            )
        })
        .collect();

    let prune = prune_directory_names(&owned);

    let mut resolved = Vec::new();
    for (extension_id, declarations) in &owned {
        for declaration in declarations {
            resolved.extend(resolve_declaration(
                worktree,
                extension_id,
                declaration,
                &prune,
            ));
        }
    }
    resolved
}

/// Directory names that are themselves declared artifacts. A declared artifact
/// tree is output, never an install scope, so nested discovery must not walk
/// into one — that is what turns scope discovery into a recursive glob.
fn prune_directory_names(owned: &[(String, Vec<ArtifactCleanupDeclaration>)]) -> HashSet<String> {
    let mut names = HashSet::new();
    for (_, declarations) in owned {
        for declaration in declarations {
            let mut components = Path::new(&declaration.path).components();
            if let Some(std::path::Component::Normal(first)) = components.next() {
                names.insert(first.to_string_lossy().to_string());
            }
        }
    }
    names
}

fn resolve_declaration(
    worktree: &Path,
    extension_id: &str,
    declaration: &ArtifactCleanupDeclaration,
    prune: &HashSet<String>,
) -> Vec<ArtifactDeclaration> {
    let mut rows = Vec::new();
    for scope in declaration.resolved_scopes() {
        for scope_dir in resolve_scope_directories(worktree, &scope, prune) {
            let Some(relative_path) = join_relative(&scope_dir, &declaration.path) else {
                continue;
            };
            if !super::is_safe_artifact_path(&relative_path) {
                continue;
            }
            rows.push(ArtifactDeclaration {
                relative_path,
                kind: declaration.id.clone(),
                declared_by: format!("extension:{extension_id}"),
                category: declaration.category.as_str().to_string(),
                reconstructable: declaration.category.is_reconstructable(),
                rehydrate_command: declaration.rehydrate_command.clone(),
                min_age_days: declaration.min_age_days,
                liveness_protected: true,
            });
        }
    }
    rows
}

/// Worktree-relative directories that qualify as install scopes for `scope`.
/// The empty string denotes the worktree root.
fn resolve_scope_directories(
    worktree: &Path,
    scope: &ArtifactCleanupScope,
    prune: &HashSet<String>,
) -> Vec<String> {
    let depth_bound = scope.depth_bound();
    let mut matches = Vec::new();
    let mut queue = VecDeque::new();
    let mut inspected = 0_usize;
    queue.push_back((PathBuf::new(), 0_usize));

    while let Some((relative, depth)) = queue.pop_front() {
        inspected += 1;
        if inspected > MAX_SCOPE_SCAN_DIRECTORIES {
            break;
        }

        let absolute = worktree.join(&relative);
        if directory_carries_manifests(&absolute, &scope.manifest_files) {
            matches.push(relative_key(&relative));
        }

        if depth >= depth_bound {
            continue;
        }
        for child in child_directories(&absolute, prune) {
            queue.push_back((relative.join(child), depth + 1));
        }
    }

    matches
}

fn directory_carries_manifests(directory: &Path, manifest_files: &[String]) -> bool {
    if !directory.is_dir() {
        return false;
    }
    manifest_files
        .iter()
        .all(|manifest| directory.join(manifest).exists())
}

/// Real child directories eligible for nested scope discovery. Symlinks are
/// excluded so resolution cannot escape the worktree, dot-directories are
/// excluded as tooling state, and declared artifact trees are pruned.
fn child_directories(directory: &Path, prune: &HashSet<String>) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut children = Vec::new();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || prune.contains(&name) {
            continue;
        }
        children.push(name);
    }
    children.sort();
    children
}

fn relative_key(relative: &Path) -> String {
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn join_relative(scope_dir: &str, path: &str) -> Option<String> {
    let path = path.trim().trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    if scope_dir.is_empty() {
        return Some(path.to_string());
    }
    Some(format!("{scope_dir}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_extension_contract::manifest_artifact_cleanup::ArtifactCleanupCategory;

    fn declaration(
        id: &str,
        path: &str,
        scopes: Vec<ArtifactCleanupScope>,
    ) -> ArtifactCleanupDeclaration {
        ArtifactCleanupDeclaration {
            id: id.to_string(),
            category: ArtifactCleanupCategory::Dependencies,
            path: path.to_string(),
            scopes,
            rehydrate_command: Some("fixture install".to_string()),
            min_age_days: None,
            description: None,
        }
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn root_scope_resolves_only_beside_declared_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scope = ArtifactCleanupScope {
            manifest_files: vec!["scope.marker".to_string()],
            nested: false,
            max_depth: None,
        };

        assert!(resolve_scope_directories(tmp.path(), &scope, &HashSet::new()).is_empty());

        write(&tmp.path().join("scope.marker"), "{}");

        assert_eq!(
            resolve_scope_directories(tmp.path(), &scope, &HashSet::new()),
            vec![String::new()]
        );
    }

    #[test]
    fn scope_without_manifest_files_resolves_the_root() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let dirs = resolve_scope_directories(
            tmp.path(),
            &ArtifactCleanupScope::default(),
            &HashSet::new(),
        );

        assert_eq!(dirs, vec![String::new()]);
    }

    #[test]
    fn nested_scopes_resolve_beside_supported_manifests_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("scope.marker"), "{}");
        write(&tmp.path().join("modules/alpha/scope.marker"), "{}");
        write(&tmp.path().join("modules/beta/unrelated.txt"), "x");

        let scope = ArtifactCleanupScope {
            manifest_files: vec!["scope.marker".to_string()],
            nested: true,
            max_depth: None,
        };

        let dirs = resolve_scope_directories(tmp.path(), &scope, &HashSet::new());

        assert!(dirs.contains(&String::new()));
        assert!(dirs.contains(&"modules/alpha".to_string()));
        assert!(!dirs.contains(&"modules/beta".to_string()));
    }

    #[test]
    fn nested_discovery_never_descends_into_a_declared_artifact_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("scope.marker"), "{}");
        write(&tmp.path().join("deps/nested/scope.marker"), "{}");

        let scope = ArtifactCleanupScope {
            manifest_files: vec!["scope.marker".to_string()],
            nested: true,
            max_depth: None,
        };
        let prune = HashSet::from(["deps".to_string()]);

        let dirs = resolve_scope_directories(tmp.path(), &scope, &prune);

        assert_eq!(dirs, vec![String::new()]);
    }

    #[test]
    fn nested_discovery_honors_the_declared_depth_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("one/scope.marker"), "{}");
        write(&tmp.path().join("one/two/scope.marker"), "{}");

        let scope = ArtifactCleanupScope {
            manifest_files: vec!["scope.marker".to_string()],
            nested: true,
            max_depth: Some(1),
        };

        let dirs = resolve_scope_directories(tmp.path(), &scope, &HashSet::new());

        assert_eq!(dirs, vec!["one".to_string()]);
    }

    #[test]
    fn declared_paths_resolve_to_scope_relative_artifact_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("scope.marker"), "{}");
        write(&tmp.path().join("modules/alpha/scope.marker"), "{}");

        let scope = ArtifactCleanupScope {
            manifest_files: vec!["scope.marker".to_string()],
            nested: true,
            max_depth: None,
        };
        let rows = resolve_declaration(
            tmp.path(),
            "fixture",
            &declaration("dependency-tree", "deps", vec![scope]),
            &HashSet::from(["deps".to_string()]),
        );

        let paths: Vec<_> = rows.iter().map(|row| row.relative_path.clone()).collect();
        assert_eq!(paths, vec!["deps", "modules/alpha/deps"]);
        assert!(rows
            .iter()
            .all(|row| row.declared_by == "extension:fixture"));
        assert!(rows.iter().all(|row| row.kind == "dependency-tree"));
        assert!(rows.iter().all(|row| row.liveness_protected));
        assert!(rows.iter().all(|row| row.reconstructable));
        assert_eq!(
            rows[0].rehydrate_command.as_deref(),
            Some("fixture install")
        );
    }

    #[test]
    fn declared_paths_escaping_the_worktree_are_dropped() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let rows = resolve_declaration(
            tmp.path(),
            "fixture",
            &declaration("escape", "../outside", Vec::new()),
            &HashSet::new(),
        );

        assert!(rows.is_empty());
    }

    #[test]
    fn artifact_path_first_component_is_pruned_from_scope_discovery() {
        let owned = vec![(
            "fixture".to_string(),
            vec![declaration("dependency-tree", "deps/inner", Vec::new())],
        )];

        assert_eq!(
            prune_directory_names(&owned),
            HashSet::from(["deps".to_string()])
        );
    }
}
