use crate::core::component::{resolve_component_scope, Component, ScopeCommand};
use crate::core::error::Result;
use crate::core::git;

#[derive(Debug, Clone)]
pub(super) struct ReleaseScope {
    pub git_root: String,
    pub component_path: String,
    pub path_prefix: String,
    pub tag_prefix: Option<String>,
    pub path_prefixes: Vec<String>,
}

impl ReleaseScope {
    pub fn resolve(component: &Component, component_id: &str) -> Result<Self> {
        let git_root = git::get_git_root(&component.local_path)
            .unwrap_or_else(|_| component.local_path.clone());
        let detected_prefix = git::get_component_path_prefix(&component.local_path);
        let mut path_prefixes = release_scope_prefixes(component);

        if let Some(prefix) = detected_prefix.as_ref() {
            for scoped_prefix in &mut path_prefixes {
                let component_prefix = prefix.trim_end_matches('/');
                if *scoped_prefix == component_prefix
                    || scoped_prefix.starts_with(&format!("{}/", component_prefix))
                {
                    continue;
                }
                *scoped_prefix = format!("{}/{}", component_prefix, scoped_prefix);
            }
            if !path_prefixes.contains(prefix) {
                path_prefixes.push(prefix.clone());
            }
        }

        path_prefixes.sort();
        path_prefixes.dedup();

        let path_prefix = detected_prefix
            .or_else(|| path_prefixes.first().cloned())
            .unwrap_or_default();
        let tag_prefix = (!path_prefix.is_empty()).then(|| component_id.to_string());

        Ok(Self {
            git_root,
            component_path: component.local_path.clone(),
            path_prefix,
            tag_prefix,
            path_prefixes,
        })
    }

    pub fn tag_name(&self, version: &str) -> String {
        match self.tag_prefix.as_deref() {
            Some(prefix) => format!("{}-v{}", prefix, version),
            None => format!("v{}", version),
        }
    }

    pub fn tag_prefix(&self) -> Option<&str> {
        self.tag_prefix.as_deref()
    }

    pub fn latest_tag(&self) -> Result<Option<String>> {
        git::get_latest_tag_with_prefix(&self.git_root, self.tag_prefix())
    }

    pub fn latest_tag_any(&self) -> Result<Option<String>> {
        git::get_latest_tag_any_with_prefix(&self.git_root, self.tag_prefix())
    }

    #[allow(dead_code)]
    pub fn previous_tag_before(&self, tag: &str) -> Result<Option<String>> {
        git::get_previous_tag_before_with_prefix(&self.git_root, tag, self.tag_prefix())
    }

    pub fn previous_tag_before_any(&self, tag: &str) -> Result<Option<String>> {
        git::get_previous_tag_before_any_with_prefix(&self.git_root, tag, self.tag_prefix())
    }

    pub fn commits_since_latest_tag(&self) -> Result<(Option<String>, Vec<git::CommitInfo>)> {
        git::fetch_tags(&self.git_root)?;
        let latest_tag = self.latest_tag()?;
        let path_prefixes: Vec<&str> = self.path_prefixes.iter().map(String::as_str).collect();
        let commits = git::get_commits_since_tag_for_paths(
            &self.git_root,
            latest_tag.as_deref(),
            &path_prefixes,
        )?;
        Ok((latest_tag, commits))
    }
}

fn release_scope_prefixes(component: &Component) -> Vec<String> {
    let scope = resolve_component_scope(component, ScopeCommand::Release);
    let mut prefixes: Vec<String> = scope
        .include
        .iter()
        .filter_map(|path| normalize_release_scope_path(path))
        .collect();

    if prefixes.is_empty() {
        if let Some(prefix) = infer_common_release_prefix(component) {
            prefixes.push(prefix);
        }
    }

    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn infer_common_release_prefix(component: &Component) -> Option<String> {
    let mut paths = Vec::new();

    if let Some(targets) = component.version_targets.as_ref() {
        paths.extend(
            targets
                .iter()
                .filter_map(|target| normalize_release_scope_path(&target.file)),
        );
    }

    if let Some(target) = component.changelog_target.as_ref() {
        if let Some(path) = normalize_release_scope_path(target) {
            paths.push(path);
        }
    }

    common_directory_prefix(&paths)
}

fn normalize_release_scope_path(path: &str) -> Option<String> {
    let mut value = path.trim().trim_start_matches("./").trim_matches('/');
    if value.is_empty() || value == "." {
        return None;
    }

    if let Some(wildcard) = value.find('*') {
        value = value[..wildcard].trim_end_matches('/');
    }

    if value.is_empty() || value == "." {
        return None;
    }

    Some(value.to_string())
}

fn common_directory_prefix(paths: &[String]) -> Option<String> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut prefix: Vec<&str> = first.split('/').collect();
    if prefix.len() <= 1 {
        return None;
    }
    prefix.pop();

    for path in iter {
        let mut dirs: Vec<&str> = path.split('/').collect();
        if dirs.len() <= 1 {
            return None;
        }
        dirs.pop();

        let keep = prefix
            .iter()
            .zip(dirs.iter())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(keep);
        if prefix.is_empty() {
            return None;
        }
    }

    Some(prefix.join("/"))
}

#[cfg(test)]
mod tests {
    use super::ReleaseScope;
    use crate::core::component::{CommandScopeConfig, Component, ScopeConfig};

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_file(dir: &std::path::Path, name: &str, content: &str, message: &str) {
        if let Some(parent) = dir.join(name).parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(dir.join(name), content).expect("write fixture file");
        run_git(dir, &["add", name]);
        run_git(dir, &["commit", "-q", "-m", message]);
    }

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Homeboy Test"]);
        temp
    }

    #[test]
    fn blocks_engine_style_package_scope_uses_package_tags_and_ignores_siblings() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "php-transformer-v0.2.2"]);
        run_git(dir, &["tag", "figma-transformer-v0.1.2"]);
        run_git(dir, &["tag", "blocks-engine-v0.2.2"]);
        commit_file(
            dir,
            "figma-transformer/src/index.ts",
            "figma",
            "fix: update figma transformer",
        );
        commit_file(
            dir,
            "php-transformer/src/index.php",
            "php",
            "fix: update php transformer",
        );

        let component = Component {
            id: "php-transformer".to_string(),
            local_path: dir.join("php-transformer").to_string_lossy().to_string(),
            ..Default::default()
        };

        let scope = ReleaseScope::resolve(&component, "php-transformer").expect("release scope");
        let (latest_tag, commits) = scope.commits_since_latest_tag().expect("commits");

        assert_eq!(scope.component_path, component.local_path);
        assert_eq!(scope.path_prefix, "php-transformer");
        assert_eq!(scope.tag_name("0.2.3"), "php-transformer-v0.2.3");
        assert_eq!(latest_tag.as_deref(), Some("php-transformer-v0.2.2"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: update php transformer");
    }

    #[test]
    fn homeboy_extensions_style_root_and_package_tags_stay_in_their_namespaces() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v2.9.3"]);
        run_git(dir, &["tag", "wordpress-v3.22.1"]);
        run_git(dir, &["tag", "nodejs-v2.2.0"]);
        run_git(dir, &["tag", "rust-v1.22.3"]);
        commit_file(dir, "README.md", "root", "fix: update root package");
        run_git(dir, &["tag", "v2.10.0"]);
        commit_file(
            dir,
            "packages/wordpress/src/index.ts",
            "wordpress",
            "fix: update wordpress package",
        );

        let root = Component {
            id: "homeboy-extensions".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let wordpress = Component {
            id: "wordpress".to_string(),
            local_path: dir.join("packages/wordpress").to_string_lossy().to_string(),
            ..Default::default()
        };

        let root_scope = ReleaseScope::resolve(&root, "homeboy-extensions").expect("root scope");
        let wordpress_scope =
            ReleaseScope::resolve(&wordpress, "wordpress").expect("package scope");

        assert_eq!(root_scope.latest_tag().unwrap().as_deref(), Some("v2.10.0"));
        assert_eq!(
            root_scope
                .previous_tag_before("v2.10.0")
                .unwrap()
                .as_deref(),
            Some("v2.9.3")
        );
        assert_eq!(root_scope.tag_name("2.10.1"), "v2.10.1");
        assert_eq!(
            wordpress_scope.latest_tag().unwrap().as_deref(),
            Some("wordpress-v3.22.1")
        );
        assert_eq!(wordpress_scope.tag_name("3.22.2"), "wordpress-v3.22.2");
        assert_eq!(
            wordpress_scope
                .previous_tag_before_any("wordpress-v3.22.2")
                .unwrap()
                .as_deref(),
            Some("wordpress-v3.22.1")
        );
    }

    #[test]
    fn repo_root_component_with_release_scope_gets_package_tag_namespace() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "blocks-engine-v0.2.2"]);
        commit_file(
            dir,
            "packages/blocks-engine/src/index.ts",
            "blocks-engine",
            "fix: update blocks engine package",
        );

        let component = Component {
            id: "blocks-engine".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            scopes: Some(ScopeConfig {
                release: Some(CommandScopeConfig {
                    include: vec!["packages/blocks-engine/**".to_string()],
                    exclude: vec![],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let scope = ReleaseScope::resolve(&component, "blocks-engine").expect("release scope");
        let (latest_tag, commits) = scope.commits_since_latest_tag().expect("commits");

        assert_eq!(scope.path_prefix, "packages/blocks-engine");
        assert_eq!(scope.tag_name("0.2.3"), "blocks-engine-v0.2.3");
        assert_eq!(latest_tag.as_deref(), Some("blocks-engine-v0.2.2"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: update blocks engine package");
    }
}
