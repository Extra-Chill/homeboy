use super::{CommandScopeConfig, Component};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeCommand {
    Lint,
    Test,
    Deploy,
    Release,
}

#[derive(Debug, Clone, Default)]
pub struct EffectiveScope {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

pub fn resolve_component_scope(component: &Component, command: ScopeCommand) -> EffectiveScope {
    let mut effective = EffectiveScope::default();

    if matches!(command, ScopeCommand::Lint | ScopeCommand::Test) {
        discover_nested_components(
            std::path::Path::new(&component.local_path),
            &mut effective.exclude,
        );
    }

    let Some(scopes) = component.scopes.as_ref() else {
        dedupe(&mut effective.exclude);
        return effective;
    };

    if let Some(defaults) = scopes.defaults.as_ref() {
        merge_scope(&mut effective, defaults);
    }

    let command_scope = match command {
        ScopeCommand::Lint => scopes.lint.as_ref(),
        ScopeCommand::Test => scopes.test.as_ref(),
        ScopeCommand::Deploy => scopes.deploy.as_ref(),
        ScopeCommand::Release => scopes.release.as_ref(),
    };

    if let Some(scope) = command_scope {
        merge_scope(&mut effective, scope);
    }

    dedupe(&mut effective.include);
    dedupe(&mut effective.exclude);
    effective
}

impl EffectiveScope {
    /// Match component-relative paths (or absolute paths beneath `root`).
    /// Plain paths exclude the entire subtree; glob patterns use glob-match.
    pub fn excludes_path(&self, root: &std::path::Path, file: &str) -> bool {
        let path = std::path::Path::new(file);
        let relative = if path.is_absolute() {
            let Ok(relative) = path.strip_prefix(root) else {
                return false;
            };
            relative
        } else {
            path
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        let path = path.strip_prefix("./").unwrap_or(&path);
        self.exclude.iter().any(|pattern| {
            let pattern = pattern
                .strip_prefix("./")
                .unwrap_or(pattern)
                .trim_end_matches('/');
            if pattern.is_empty() {
                return false;
            }
            path == pattern
                || path
                    .strip_prefix(pattern)
                    .is_some_and(|tail| tail.starts_with('/'))
                || glob_match::glob_match(pattern, path)
        })
    }

    /// Keep locationless diagnostics: they cannot safely be attributed to an excluded path.
    pub fn filter_findings(
        &self,
        root: &std::path::Path,
        findings: &mut Vec<crate::finding::HomeboyFinding>,
    ) {
        findings.retain(|finding| {
            finding
                .location
                .file
                .as_deref()
                .is_none_or(|file| !self.excludes_path(root, file))
        });
    }
}

/// Resolve against the actual execution checkout, including path overrides/worktrees.
pub fn resolve_component_scope_at(
    component: &Component,
    command: ScopeCommand,
    root: &std::path::Path,
) -> EffectiveScope {
    let mut component = component.clone();
    component.local_path = root.to_string_lossy().into_owned();
    resolve_component_scope(&component, command)
}

fn discover_nested_components(root: &std::path::Path, excludes: &mut Vec<String>) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // Do not follow symlinks (cycles or directories outside the component).
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if matches!(
                entry.file_name().to_str(),
                Some(".git" | "node_modules" | "vendor" | "target")
            ) {
                continue;
            }
            let path = entry.path();
            if path.join("homeboy.json").is_file() {
                if let Ok(relative) = path.strip_prefix(root) {
                    excludes.push(format!(
                        "{}/",
                        relative.to_string_lossy().replace('\\', "/")
                    ));
                }
            } else {
                pending.push(path);
            }
        }
    }
}

fn merge_scope(target: &mut EffectiveScope, scope: &CommandScopeConfig) {
    target.include.extend(scope.include.iter().cloned());
    target.exclude.extend(scope.exclude.iter().cloned());
}

fn dedupe(items: &mut Vec<String>) {
    items.sort();
    items.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, ScopeConfig};

    #[test]
    fn merges_default_and_command_specific_excludes() {
        let mut component = Component::new(
            "homeboy".to_string(),
            "/tmp/homeboy".to_string(),
            "".to_string(),
            None,
        );
        component.scopes = Some(ScopeConfig {
            defaults: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["tmp/**".to_string(), "docs/CHANGES.md".to_string()],
            }),
            deploy: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["docs/CHANGES.md".to_string()],
            }),
            ..Default::default()
        });

        let resolved = resolve_component_scope(&component, ScopeCommand::Deploy);
        assert_eq!(resolved.exclude, vec!["docs/CHANGES.md", "tmp/**"]);
    }

    #[test]
    fn lint_test_scope_excludes_nested_components_and_filters_findings() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir_all(root.join("plugins/child/deeper")).unwrap();
        std::fs::write(root.join("homeboy.json"), "{}").unwrap();
        std::fs::write(root.join("plugins/child/homeboy.json"), "{}").unwrap();
        std::fs::write(root.join("plugins/child/deeper/homeboy.json"), "{}").unwrap();
        let mut component = Component::new(
            "root".into(),
            root.to_string_lossy().into_owned(),
            "".into(),
            None,
        );
        component.scopes = Some(ScopeConfig {
            defaults: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["packages/".into()],
            }),
            lint: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["lint-only/**".into()],
            }),
            test: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["test-only/".into()],
            }),
            ..Default::default()
        });
        for (command, specific, other) in [
            (
                ScopeCommand::Lint,
                "lint-only/file.php",
                "test-only/file.php",
            ),
            (
                ScopeCommand::Test,
                "test-only/file.php",
                "lint-only/file.php",
            ),
        ] {
            let scope = resolve_component_scope(&component, command);
            assert!(scope.exclude.contains(&"plugins/child/".to_string()));
            assert!(scope.excludes_path(root, specific));
            assert!(!scope.excludes_path(root, other));
            assert!(!scope.excludes_path(root, "packages-extra/file.php"));
            let mut findings = [
                "plugins/child/file.php",
                "packages/file.php",
                specific,
                "plugins/sibling/file.php",
                "inc/root.php",
            ]
            .into_iter()
            .map(|file| {
                crate::finding::HomeboyFinding::builder("lint", "fixture")
                    .file(file)
                    .build()
            })
            .collect::<Vec<_>>();
            findings.push(crate::finding::HomeboyFinding::builder("test", "global").build());
            findings.push(
                crate::finding::HomeboyFinding::builder("test", "absolute")
                    .file(root.join("plugins/child/file.php").to_string_lossy())
                    .build(),
            );
            scope.filter_findings(root, &mut findings);
            assert_eq!(findings.len(), 3);
            assert_eq!(
                findings[0].location.file.as_deref(),
                Some("plugins/sibling/file.php")
            );
            assert_eq!(findings[1].location.file.as_deref(), Some("inc/root.php"));
            assert!(findings[2].location.file.is_none());
        }
        // Automatic boundaries apply even with no explicit scope configuration.
        component.scopes = None;
        for command in [ScopeCommand::Lint, ScopeCommand::Test] {
            assert_eq!(
                resolve_component_scope(&component, command).exclude,
                vec!["plugins/child/"]
            );
        }
        assert!(resolve_component_scope(&component, ScopeCommand::Deploy)
            .exclude
            .is_empty());
        // A worktree override is scanned instead of the registered source checkout.
        let worktree = tempfile::tempdir().unwrap();
        assert!(
            resolve_component_scope_at(&component, ScopeCommand::Lint, worktree.path())
                .exclude
                .is_empty()
        );
    }

    #[test]
    fn scope_is_empty_without_component_scopes() {
        let component = Component::new(
            "generic".to_string(),
            "/tmp/generic".to_string(),
            "".to_string(),
            None,
        );

        let resolved = resolve_component_scope(&component, ScopeCommand::Deploy);
        assert!(resolved.exclude.is_empty());
        assert!(resolved.include.is_empty());
    }
}
