//! Registry-independent changed-component detection for a checked-out monorepo.
use super::scope::ScopeCommand;
use crate::{Error, Result};
use serde::Serialize;
use std::{collections::BTreeSet, path::Path, process::Command};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChangedComponent {
    pub component: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ChangedComponents {
    pub include: Vec<ChangedComponent>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("changed", message.into(), None, None)
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| invalid(e.to_string()))?;
    if !output.status.success() {
        return Err(invalid(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(output.stdout)
}

fn paths(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8(p.to_vec()).map_err(|_| invalid("Non-UTF-8 repository path")))
        .collect()
}

fn matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
    path == pattern
        || path.starts_with(&format!("{pattern}/"))
        || glob_match::glob_match(pattern, path)
}

/// Discover manifests in the checkout (tracked or unignored), then compare two
/// commits. Checkout `head` first: component topology/config comes from disk.
/// Shared patterns are repository-relative globs and override component scopes.
pub fn changed(
    repo: &Path,
    base: &str,
    head: &str,
    shared: &[String],
) -> Result<ChangedComponents> {
    let root = git(repo, &["rev-parse", "--show-toplevel"])?;
    let root = std::path::PathBuf::from(
        String::from_utf8(root)
            .map_err(|_| invalid("Non-UTF-8 repository root"))?
            .trim(),
    );
    // Resolve refs before passing them to diff so option-like input cannot be
    // interpreted as flags. Invalid/missing revisions are errors, never empty sets.
    let resolve = |reference: &str| -> Result<String> {
        let bytes = git(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    };
    let base = resolve(base)?;
    let head = resolve(head)?;
    let files = paths(&git(
        &root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?)?;
    let mut components = Vec::new();
    let mut ids = BTreeSet::new();
    let mut manifests = BTreeSet::new();
    for file in files {
        let path = Path::new(&file);
        if path.file_name().and_then(|s| s.to_str()) != Some("homeboy.json")
            || !root.join(path).is_file()
        {
            continue;
        }
        manifests.insert(path.parent().unwrap_or(Path::new("")).to_path_buf());
    }
    for dir in manifests {
        let source = root.join(&dir);
        let bytes =
            std::fs::read(source.join("homeboy.json")).map_err(|e| invalid(e.to_string()))?;
        let component = super::portable::component_from_portable_bytes(&bytes, &source)?;
        let id = component.id.clone();
        if !ids.insert(id.clone()) {
            return Err(invalid(format!("Duplicate component id: {id}")));
        }
        let mut excludes =
            super::resolve_component_scope(&component, ScopeCommand::Release).exclude;
        excludes.extend(super::resolve_component_scope(&component, ScopeCommand::Deploy).exclude);
        let dependencies = crate::hygiene::validation_dependency_ids(&source)?;
        components.push((
            ChangedComponent {
                component: id,
                path: if dir.as_os_str().is_empty() {
                    ".".into()
                } else {
                    dir.to_string_lossy().replace('\\', "/")
                },
            },
            excludes,
            dependencies,
        ));
    }
    components.sort_by(|a, b| a.0.component.cmp(&b.0.component));
    let changed_paths = paths(&git(
        &root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &base,
            &head,
            "--",
        ],
    )?)?;
    let mut selected = BTreeSet::new();
    for path in changed_paths {
        if shared.iter().any(|p| matches(p, &path)) {
            selected.extend(0..components.len());
            break;
        }
        // Ownership is established BEFORE exclusions. An excluded nested path
        // must not fall through and accidentally select the root component.
        let owner = components
            .iter()
            .enumerate()
            .filter(|(_, (c, _, _))| c.path == "." || path.starts_with(&format!("{}/", c.path)))
            .max_by_key(|(_, (c, _, _))| if c.path == "." { 0 } else { c.path.len() });
        if let Some((index, (component, excludes, _))) = owner {
            let relative = if component.path == "." {
                &path
            } else {
                &path[component.path.len() + 1..]
            };
            if !excludes.iter().any(|p| matches(p, relative)) {
                selected.insert(index);
            }
        }
    }
    loop {
        let mut added = Vec::new();
        for (index, (component, _, deps)) in components.iter().enumerate() {
            if selected.contains(&index) {
                continue;
            }
            if deps.iter().any(|dep| {
                selected.iter().any(|i| {
                    let target = &components[*i].0;
                    if dep == &target.component {
                        return true;
                    }
                    // Path dependencies are relative to the declaring component;
                    // resolve only against discovered same-repo roots, not registry.
                    let from = root.join(&component.path).join(dep).canonicalize().ok();
                    from.is_some() && from == root.join(&target.path).canonicalize().ok()
                })
            }) {
                added.push(index);
            }
        }
        if added.is_empty() {
            break;
        }
        selected.extend(added);
    }
    Ok(ChangedComponents {
        include: selected
            .into_iter()
            .map(|i| components[i].0.clone())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn commit(root: &Path) -> String {
        git(root, &["add", "."]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "test",
            ],
        )
        .unwrap();
        String::from_utf8(git(root, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .into()
    }

    #[test]
    fn changed_short_nested_root_and_unclaimed_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q"]).unwrap();
        fs::write(root.join("homeboy.json"), r#"{"id":"root"}"#).unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/homeboy.json"), r#"{"id":"child"}"#).unwrap();
        let base = commit(root);
        fs::write(root.join("a/file"), "child").unwrap();
        let head = commit(root);
        assert_eq!(
            changed(root, &base, &head, &[]).unwrap().include,
            vec![ChangedComponent {
                component: "child".into(),
                path: "a".into()
            }]
        );
        fs::remove_file(root.join("homeboy.json")).unwrap();
        let base = commit(root);
        fs::write(root.join("unclaimed"), "no owner").unwrap();
        let head = commit(root);
        assert!(changed(root, &base, &head, &[]).unwrap().include.is_empty());
        assert_eq!(
            changed(root, &base, &head, &["unclaimed".into()])
                .unwrap()
                .include
                .len(),
            1
        );
    }

    #[test]
    fn changed_monorepo_ownership_dependencies_shared_and_matrix() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q"]).unwrap();
        for (dir, config) in [
            (
                ".",
                serde_json::json!({"id":"network", "scopes":{"release":{"exclude":["docs/**"]}}}),
            ),
            (
                "plugins/a",
                serde_json::json!({"id":"a", "scopes":{"release":{"exclude":["ignored/**"]}}}),
            ),
            ("plugins/a/nested", serde_json::json!({"id":"nested"})),
            (
                "themes/theme",
                serde_json::json!({"id":"theme", "validation_dependencies":["a"]}),
            ),
            (
                "packages/ui",
                serde_json::json!({"id":"ui", "validation_dependencies":["../../themes/theme", "external"]}),
            ),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("homeboy.json"), config.to_string()).unwrap();
        }
        let mut base = commit(root);
        for (file, expected) in [
            ("plugins/a/nested/code.php", vec!["nested"]),
            ("plugins/a/code.php", vec!["a", "theme", "ui"]),
            ("plugins/a/ignored/test.txt", vec![]),
            ("docs/readme.md", vec![]),
            ("network.php", vec!["network"]),
        ] {
            fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            fs::write(root.join(file), "change").unwrap();
            let head = commit(root);
            let output = changed(root, &base, &head, &[]).unwrap();
            assert_eq!(
                output
                    .include
                    .iter()
                    .map(|c| c.component.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{file}"
            );
            base = head;
        }
        fs::create_dir_all(root.join(".github/workflows")).unwrap();
        fs::write(root.join(".github/workflows/deploy.yml"), "shared").unwrap();
        let head = commit(root);
        let output = changed(root, &base, &head, &[".github/**".into()]).unwrap();
        assert_eq!(
            serde_json::to_value(output).unwrap(),
            serde_json::json!({"include":[
                {"component":"a","path":"plugins/a"},
                {"component":"nested","path":"plugins/a/nested"},
                {"component":"network","path":"."},
                {"component":"theme","path":"themes/theme"},
                {"component":"ui","path":"packages/ui"}
            ]})
        );
        // Renames must select both the old and new owner; deletions retain
        // ownership even though the changed file no longer exists.
        fs::rename(
            root.join("plugins/a/nested/code.php"),
            root.join("network-moved.php"),
        )
        .unwrap();
        let renamed = commit(root);
        let output = changed(root, &head, &renamed, &[]).unwrap();
        assert_eq!(
            output
                .include
                .iter()
                .map(|c| c.component.as_str())
                .collect::<Vec<_>>(),
            vec!["nested", "network"]
        );
        fs::remove_file(root.join("network-moved.php")).unwrap();
        let deleted = commit(root);
        assert_eq!(
            changed(root, &renamed, &deleted, &[]).unwrap().include[0].component,
            "network"
        );
        assert!(changed(root, "missing-ref", &head, &[]).is_err());
        assert!(changed(root, &head, &head, &[]).unwrap().include.is_empty());
    }
}
