use crate::core::engine::identifier;
use crate::core::error::{Error, Result};
use crate::core::paths;
use std::path::{Path, PathBuf};

use super::load_extension;

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallForComponentResult {
    pub component_id: String,
    pub source: String,
    pub installed: Vec<InstallResult>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub linked: bool,
    pub source_path: Option<PathBuf>,
    pub git_root: Option<PathBuf>,
    pub source_update: super::ExtensionSourceUpdate,
    pub repaired_source_metadata: Option<source_metadata::SourceMetadataRepair>,
}

pub mod source_metadata;

pub fn slugify_id(value: &str) -> Result<String> {
    identifier::slugify_id(value, "extension_id")
}

/// Derive a extension ID from a git URL.
pub fn derive_id_from_url(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    let segment = trimmed
        .split('/')
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");

    slugify_id(segment)
}

/// Check if a string looks like a git URL (vs a local path).
pub fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
}

/// Returns the path to a extension's manifest file: {extension_dir}/{id}.json
fn manifest_path_for_extension(extension_dir: &Path, id: &str) -> PathBuf {
    extension_dir.join(format!("{}.json", id))
}

/// Install a extension from a git URL or link a local directory.
/// Automatically detects whether source is a URL (git clone) or local path (symlink).
pub fn install(source: &str, id_override: Option<&str>) -> Result<InstallResult> {
    install_with_revision(source, id_override, None)
}

/// Install a extension from a git URL or link a local directory.
/// Git URL installs optionally check out a branch, tag, or commit after cloning.
pub fn install_with_revision(
    source: &str,
    id_override: Option<&str>,
    revision: Option<&str>,
) -> Result<InstallResult> {
    if is_git_url(source) {
        install_from_url(source, id_override, revision)
    } else {
        install_from_path(source, id_override, None)
    }
}

/// Install every extension declared by a component from the same source.
///
/// Already-installed extensions are skipped so CI setup can be re-run safely.
pub fn install_for_component(
    component: &crate::core::component::Component,
    source: &str,
) -> Result<InstallForComponentResult> {
    let extensions = component.extensions.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no extensions configured", component.id),
            Some(component.id.clone()),
            None,
        )
    })?;

    if extensions.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no extensions configured", component.id),
            Some(component.id.clone()),
            None,
        ));
    }

    let mut extension_ids: Vec<String> = extensions.keys().cloned().collect();
    extension_ids.sort();

    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    for extension_id in extension_ids {
        if load_extension(&extension_id).is_ok() {
            skipped.push(extension_id);
            continue;
        }

        installed.push(install_configured_extension(source, &extension_id)?);
    }

    Ok(InstallForComponentResult {
        component_id: component.id.clone(),
        source: source.to_string(),
        installed,
        skipped,
    })
}

#[derive(Debug, Clone)]
pub struct RefreshResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub source_revision: Option<String>,
    /// Whether a previous install was removed before reinstalling. False on a
    /// first-time install (nothing to uninstall).
    pub uninstalled_previous: bool,
}

/// Refresh a single extension from a source: uninstall any existing install,
/// then reinstall from the given source/revision.
///
/// This is the core-owned replacement for CI's hardcoded
/// "uninstall (if present) then install" shell sequence. It is idempotent and
/// safe to re-run: a missing prior install is not an error.
pub fn refresh(
    source: &str,
    id_override: Option<&str>,
    revision: Option<&str>,
) -> Result<RefreshResult> {
    let extension_id = match id_override {
        Some(id) => slugify_id(id)?,
        None => derive_id_from_url(source)?,
    };

    let uninstalled_previous = if load_extension(&extension_id).is_ok() {
        uninstall(&extension_id)?;
        true
    } else {
        false
    };

    let installed = install_with_revision(source, Some(&extension_id), revision)?;

    Ok(RefreshResult {
        extension_id: installed.extension_id,
        url: installed.url,
        path: installed.path,
        manifest_path: installed.manifest_path,
        source_revision: installed.source_revision,
        uninstalled_previous,
    })
}

mod install_sources;
use install_sources::{install_configured_extension, install_from_path, install_from_url};
pub(crate) use install_sources::{
    install_linked_shared_assets, rename_dir, resolve_cloned_extension,
};

mod update;
#[cfg(test)]
use update::is_extension_update_workdir_clean;
pub(crate) use update::write_source_metadata;
pub use update::{check_update_available, read_source_revision, update, UpdateAvailable};

/// Uninstall a extension. Automatically detects symlinks vs cloned directories.
/// - Symlinked extensions: removes symlink only (source preserved)
/// - Cloned extensions: removes directory entirely
pub fn uninstall(extension_id: &str) -> Result<PathBuf> {
    let extension_dir = paths::extension(extension_id)?;
    if !extension_dir.exists() {
        return Err(Error::extension_not_found(extension_id.to_string(), vec![]));
    }

    if extension_dir.is_symlink() {
        // Symlinked extension: just remove the symlink, source directory is preserved
        std::fs::remove_file(&extension_dir)
            .map_err(|e| Error::internal_io(e.to_string(), Some("remove symlink".to_string())))?;
    } else {
        // Cloned extension: remove the directory
        std::fs::remove_dir_all(&extension_dir).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("remove extension directory".to_string()),
            )
        })?;
    }

    Ok(extension_dir)
}

#[cfg(test)]
mod tests {
    use super::{
        install, install_for_component, install_with_revision, is_extension_update_workdir_clean,
        load_extension, read_source_revision, refresh, source_metadata, update,
    };
    use crate::core::component;
    use crate::core::extension::update_all;
    use crate::test_support::with_isolated_home;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn write_extension_fixture(root: &Path, id: &str) {
        write_extension_fixture_with_version(root, id, "1.0.0");
    }

    fn write_extension_fixture_with_version(root: &Path, id: &str, version: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "{}"
}}"#,
                id, version
            ),
        )
        .expect("extension manifest");
    }

    fn write_extension_fixture_with_setup(root: &Path, id: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{"name":"{} extension","version":"1.0.0","executable":{{"runtime":{{"setup_command":"printf setup >> setup-count.txt"}}}}}}"#,
                id
            ),
        )
        .expect("extension manifest");
    }

    fn write_extension_fixture_with_agent_runtime_provider(root: &Path, id: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "1.0.0",
  "agent_runtimes": [{{
    "id": "{}-runtime",
    "agent_task_executors": [{{
      "id": "{}.default",
      "backend": "{}"
    }}]
  }}]
}}"#,
                id, id, id, id
            ),
        )
        .expect("extension manifest");
    }

    fn write_extension_fixture_with_invalid_agent_runtime_provider(root: &Path, id: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "1.0.0",
  "agent_runtimes": [{{
    "id": "{}-runtime",
    "agent_task_executors": [{{
      "id": "{}.default"
    }}]
  }}]
}}"#,
                id, id, id
            ),
        )
        .expect("extension manifest");
    }

    fn write_component_fixture(root: &Path, extensions: &[&str]) {
        let extension_json = extensions
            .iter()
            .map(|id| format!(r#"    "{}": {{}}"#, id))
            .collect::<Vec<_>>()
            .join(",\n");

        fs::write(
            root.join("homeboy.json"),
            format!(
                r#"{{
  "id": "multi-extension-component",
  "extensions": {{
{}
  }}
}}"#,
                extension_json
            ),
        )
        .expect("component config");
    }

    fn write_shared_runtime_fixture(root: &Path) {
        let runtime_script = root.join(
            "agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs",
        );
        fs::create_dir_all(runtime_script.parent().expect("runtime script parent"))
            .expect("runtime script dir");
        fs::write(&runtime_script, "console.log('sample runtime');\n").expect("runtime script");

        let runtime_agent_ci_helper =
            root.join("runtime-agent-ci/lib/agent-task-provider-contract.js");
        fs::create_dir_all(
            runtime_agent_ci_helper
                .parent()
                .expect("runtime agent ci helper parent"),
        )
        .expect("runtime agent ci helper dir");
        fs::write(
            &runtime_agent_ci_helper,
            "module.exports = { schema: 'fixture' };\n",
        )
        .expect("runtime agent ci helper");

        let agent_task_contract = root.join("agent-task-contracts/agent-task-provider-contract.js");
        fs::create_dir_all(
            agent_task_contract
                .parent()
                .expect("agent task contract parent"),
        )
        .expect("agent task contract dir");
        fs::write(
            &agent_task_contract,
            "module.exports = { contract: 'fixture' };\n",
        )
        .expect("agent task contract");
    }

    fn run_git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn commit_all(dir: &Path, message: &str) -> bool {
        run_git(dir, &["add", "."])
            && run_git(
                dir,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-m",
                    message,
                ],
            )
    }

    fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn prepare_git_extension_repo(repo: &Path, extension_id: &str) -> Option<TempDir> {
        write_extension_fixture(repo, extension_id);
        prepare_git_repo(repo)
    }

    fn prepare_git_extension_monorepo(repo: &Path, extension_ids: &[&str]) -> Option<TempDir> {
        for extension_id in extension_ids {
            write_extension_fixture_with_setup(repo, extension_id);
        }
        prepare_git_repo(repo)
    }

    fn prepare_git_repo(repo: &Path) -> Option<TempDir> {
        if !run_git(repo, &["init", "--quiet"]) || !commit_all(repo, "init") {
            return None;
        }

        let remote_parent = TempDir::new().expect("remote parent");
        let remote_path = remote_parent.path().join("extension.git");
        let remote_path_str = remote_path.to_string_lossy().to_string();
        if !run_git(
            repo,
            &["clone", "--bare", repo.to_str().unwrap(), &remote_path_str],
        ) {
            return None;
        }
        if !run_git(repo, &["remote", "add", "origin", &remote_path_str]) {
            return None;
        }
        if !run_git(repo, &["fetch", "origin", "--quiet"]) {
            return None;
        }
        let branch = if run_git(repo, &["rev-parse", "--verify", "main"]) {
            "main"
        } else {
            "master"
        };
        if !run_git(
            repo,
            &[
                "branch",
                "--set-upstream-to",
                &format!("origin/{branch}"),
                branch,
            ],
        ) {
            return None;
        }

        Some(remote_parent)
    }

    #[cfg(unix)]
    fn write_git_wrapper(bin_dir: &Path, pull_count_file: &Path) {
        fs::create_dir_all(bin_dir).expect("wrapper bin dir");
        let real_git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git");
        let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "pull" ]; then
  printf x >> '{}'
fi
exec '{}' "$@"
"#,
            pull_count_file.display(),
            real_git
        );
        let wrapper = bin_dir.join("git");
        fs::write(&wrapper, script).expect("git wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("wrapper perms");
    }

    #[test]
    fn test_install_for_component_installs_multiple_extensions() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);
            let component = component::discover_from_portable(&component_dir).expect("component");

            let result = install_for_component(&component, &source.to_string_lossy())
                .expect("install should succeed");

            let installed_ids = result
                .installed
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(installed_ids, vec!["alpha", "beta"]);
            assert!(result.skipped.is_empty());
            assert!(home
                .join(".config/homeboy/extensions/alpha/alpha.json")
                .exists());
            assert!(home
                .join(".config/homeboy/extensions/beta/beta.json")
                .exists());
        });
    }

    #[test]
    fn test_install_for_component_skips_already_installed_extensions() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);
            let component = component::discover_from_portable(&component_dir).expect("component");

            install(&source.join("alpha").to_string_lossy(), Some("alpha"))
                .expect("pre-install alpha");

            let result = install_for_component(&component, &source.to_string_lossy())
                .expect("install should succeed");

            let installed_ids = result
                .installed
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(installed_ids, vec!["beta"]);
            assert_eq!(result.skipped, vec!["alpha"]);
        });
    }

    #[test]
    fn test_install_for_component_uses_path_based_portable_component_config() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);

            let component = component::discover_from_portable(&component_dir)
                .expect("component should resolve from portable path");
            let result = install_for_component(&component, &source.to_string_lossy())
                .expect("install should succeed");

            assert_eq!(result.component_id, "multi-extension-component");
            assert_eq!(result.installed.len(), 2);
        });
    }

    #[test]
    fn refresh_installs_first_time_without_uninstalling() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");

            let result = refresh(&source.join("alpha").to_string_lossy(), Some("alpha"), None)
                .expect("first-time refresh should install");

            assert_eq!(result.extension_id, "alpha");
            assert!(
                !result.uninstalled_previous,
                "first-time refresh has nothing to uninstall"
            );
            assert!(load_extension("alpha").is_ok());
        });
    }

    #[test]
    fn refresh_is_idempotent_and_reinstalls_existing() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture_with_version(&source, "alpha", "1.0.0");

            install(&source.join("alpha").to_string_lossy(), Some("alpha")).expect("pre-install");

            // Update the source, then refresh — the reinstall should pick it up
            // without erroring on the pre-existing install.
            write_extension_fixture_with_version(&source, "alpha", "2.0.0");
            let result = refresh(&source.join("alpha").to_string_lossy(), Some("alpha"), None)
                .expect("refresh over existing install should succeed");

            assert!(
                result.uninstalled_previous,
                "refresh should remove the prior install before reinstalling"
            );
            assert_eq!(
                load_extension("alpha").expect("reinstalled").version,
                "2.0.0"
            );
        });
    }

    #[test]
    fn install_without_replace_remains_non_destructive() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "swift");

            install(&source.join("swift").to_string_lossy(), Some("swift"))
                .expect("initial install");

            let err = install(&source.join("swift").to_string_lossy(), Some("swift"))
                .expect_err("second install should still fail");

            assert!(err.to_string().contains("already exists"));
        });
    }

    #[test]
    fn linked_update_does_not_write_source_revision_to_source_checkout() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };

            let extension_source = source.join("wordpress");
            install(&extension_source.to_string_lossy(), Some("wordpress"))
                .expect("install linked extension");

            let before = read_source_revision("wordpress").expect("linked git revision");
            assert!(!extension_source.join(".source-revision").exists());

            update("wordpress", false).expect("update linked extension");

            assert!(
                !extension_source.join(".source-revision").exists(),
                "linked update must not write metadata into the source checkout"
            );
            assert_eq!(
                read_source_revision("wordpress"),
                Some(before),
                "linked extensions should resolve revisions through git discovery"
            );
        });
    }

    #[test]
    fn extension_update_allows_generated_source_metadata_dirty_in_linked_subdir() {
        let temp = TempDir::new().expect("create tempdir");
        let repo = temp.path();
        let extension_dir = repo.join("wordpress");
        write_extension_fixture(repo, "wordpress");
        if !run_git(repo, &["init", "--quiet"]) || !commit_all(repo, "init") {
            return;
        }

        fs::write(
            extension_dir.join(".source-url"),
            "https://example.com/ext.git",
        )
        .expect("source url metadata");
        fs::write(extension_dir.join(".source-revision"), "abc123")
            .expect("source revision metadata");

        assert!(
            is_extension_update_workdir_clean(repo, &extension_dir),
            "generated extension metadata should not block update"
        );
    }

    #[test]
    fn extension_update_blocks_user_dirty_file_next_to_generated_metadata() {
        let temp = TempDir::new().expect("create tempdir");
        let repo = temp.path();
        let extension_dir = repo.join("wordpress");
        write_extension_fixture(repo, "wordpress");
        if !run_git(repo, &["init", "--quiet"]) || !commit_all(repo, "init") {
            return;
        }

        fs::write(
            extension_dir.join(".source-url"),
            "https://example.com/ext.git",
        )
        .expect("source url metadata");
        fs::write(extension_dir.join("notes.txt"), "user work").expect("user-authored dirty file");

        assert!(
            !is_extension_update_workdir_clean(repo, &extension_dir),
            "user-authored dirt should still block update"
        );
    }

    #[test]
    fn cloned_monorepo_install_preserves_source_revision_marker() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install cloned extension");

            assert!(result.path.join(".source-revision").exists());
            assert_eq!(
                fs::read_to_string(result.path.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                remote_url.to_string_lossy()
            );
            assert_eq!(
                read_source_revision("wordpress"),
                result.source_revision,
                "monorepo installs keep the stored source revision after .git is discarded"
            );
        });
    }

    #[test]
    fn cloned_monorepo_install_materializes_shared_scripts() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "rust");
            let shared_helper = source.join("scripts/lib/test-result-adapters.sh");
            fs::create_dir_all(shared_helper.parent().expect("helper parent"))
                .expect("shared scripts dir");
            fs::write(
                &shared_helper,
                "homeboy_parse_test_results_with_adapters() { :; }\n",
            )
            .expect("shared helper");
            let remote = match prepare_git_repo(&source) {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("rust")).expect("install cloned extension");

            assert!(home
                .join(".config/homeboy/extensions/rust/rust.json")
                .exists());
            assert!(home
                .join(".config/homeboy/extensions/scripts/lib/test-result-adapters.sh")
                .exists());
        });
    }

    #[test]
    fn cloned_monorepo_install_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            write_shared_runtime_fixture(&source);
            let remote = match prepare_git_repo(&source) {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install cloned extension");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
            assert!(home
                .join(".config/homeboy/runtime-agent-ci/lib/agent-task-provider-contract.js")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-task-contracts/agent-task-provider-contract.js")
                .exists());
        });
    }

    #[test]
    fn extracted_monorepo_update_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install cloned extension");
            assert!(!home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());

            write_shared_runtime_fixture(&source);
            assert!(commit_all(&source, "add runtime package"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            update("wordpress", false).expect("update cloned extension");

            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
            assert!(home
                .join(".config/homeboy/runtime-agent-ci/lib/agent-task-provider-contract.js")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-task-contracts/agent-task-provider-contract.js")
                .exists());
        });
    }

    #[test]
    fn linked_monorepo_install_materializes_shared_scripts() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            let shared_helper = source.join("scripts/lib/test-result-adapters.sh");
            fs::create_dir_all(shared_helper.parent().expect("helper parent"))
                .expect("shared scripts dir");
            fs::write(
                &shared_helper,
                "homeboy_parse_test_results_with_adapters() { :; }\n",
            )
            .expect("shared helper");

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/extensions/scripts/lib/test-result-adapters.sh")
                .exists());
        });
    }

    #[test]
    fn linked_monorepo_install_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            write_shared_runtime_fixture(&source);

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-task-contracts/agent-task-provider-contract.js")
                .exists());
        });
    }

    #[test]
    fn linked_monorepo_root_install_with_id_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            write_shared_runtime_fixture(&source);

            install(&source.to_string_lossy(), Some("wordpress"))
                .expect("install linked extension from monorepo root");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-task-contracts/agent-task-provider-contract.js")
                .exists());
        });
    }

    #[test]
    fn linked_install_reports_manifest_path_and_discovers_declared_provider() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture_with_agent_runtime_provider(&source, "wordpress");

            let result = install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            assert_eq!(
                result.manifest_path,
                home.join(".config/homeboy/extensions/wordpress/wordpress.json")
            );
            let providers =
                crate::core::agent_runtime_manifest::discover_agent_task_executor_providers();
            assert!(providers.iter().any(|provider| {
                provider.extension_id.as_deref() == Some("wordpress")
                    && provider.runtime_id.as_deref() == Some("wordpress-runtime")
                    && provider.id == "wordpress.default"
                    && provider.backend == "wordpress"
            }));
        });
    }

    #[test]
    fn linked_install_fails_and_rolls_back_when_declared_provider_is_not_discoverable() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture_with_invalid_agent_runtime_provider(&source, "wordpress");

            let err = install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect_err("install should fail invalid provider declaration");

            assert!(err.message.contains("cannot be parsed"));
            assert!(!home.join(".config/homeboy/extensions/wordpress").exists());
        });
    }

    #[test]
    fn test_install_with_revision() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let pinned_revision = match git_output(&source, &["rev-parse", "--short", "HEAD"]) {
                Some(revision) => revision,
                None => return,
            };

            write_extension_fixture_with_version(&source, "wordpress", "2.0.0");
            assert!(commit_all(&source, "update extension"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));
            let remote_url = remote.path().join("extension.git");

            let result = install_with_revision(
                &remote_url.to_string_lossy(),
                Some("wordpress"),
                Some(&pinned_revision),
            )
            .expect("install pinned revision");

            let installed = load_extension("wordpress").expect("installed extension");
            assert_eq!(installed.version, "1.0.0");
            assert_eq!(
                result.source_revision.as_deref(),
                Some(pinned_revision.as_str())
            );
            assert_eq!(
                read_source_revision("wordpress").as_deref(),
                Some(pinned_revision.as_str())
            );
        });
    }

    #[test]
    fn cloned_install_can_checkout_requested_branch() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            assert!(run_git(&source, &["checkout", "-b", "next-extension"]));
            write_extension_fixture_with_version(&source, "wordpress", "2.0.0");
            assert!(commit_all(&source, "branch extension update"));
            assert!(run_git(&source, &["push", "origin", "next-extension"]));
            let branch_revision = match git_output(&source, &["rev-parse", "--short", "HEAD"]) {
                Some(revision) => revision,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install_with_revision(
                &remote_url.to_string_lossy(),
                Some("wordpress"),
                Some("next-extension"),
            )
            .expect("install branch revision");

            let installed = load_extension("wordpress").expect("installed extension");
            assert_eq!(installed.version, "2.0.0");
            assert_eq!(
                result.source_revision.as_deref(),
                Some(branch_revision.as_str())
            );
        });
    }

    #[test]
    fn update_all_updates_linked_extensions_through_single_update_path() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            let result = update_all(false);

            assert_eq!(result.updated.len(), 1);
            assert_eq!(result.updated[0].extension_id, "wordpress");
            assert!(
                result.skipped.is_empty(),
                "linked extensions should not be pre-skipped by update_all"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn test_run_setup_if_configured() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_monorepo(&source, &["fixture-a", "fixture-b"])
            {
                Some(remote) => remote,
                None => return,
            };

            install(
                &source.join("fixture-a").to_string_lossy(),
                Some("fixture-a"),
            )
            .expect("install linked fixture-a extension");
            install(
                &source.join("fixture-b").to_string_lossy(),
                Some("fixture-b"),
            )
            .expect("install linked fixture-b extension");

            let bin_dir = home.join("bin");
            let pull_count_file = home.join("pull-count");
            write_git_wrapper(&bin_dir, &pull_count_file);
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), old_path));

            let result = update_all(false);

            std::env::set_var("PATH", old_path);

            let updated_ids = result
                .updated
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(updated_ids, vec!["fixture-a", "fixture-b"]);
            assert!(result.skipped.is_empty());
            assert_eq!(
                fs::read_to_string(&pull_count_file)
                    .unwrap_or_default()
                    .len(),
                1,
                "linked extensions sharing one git root should run one git pull"
            );
            assert_eq!(
                fs::read_to_string(source.join("fixture-a/setup-count.txt"))
                    .unwrap_or_default()
                    .matches("setup")
                    .count(),
                1,
                "fixture-a setup should still run after the shared root update"
            );
            assert_eq!(
                fs::read_to_string(source.join("fixture-b/setup-count.txt"))
                    .unwrap_or_default()
                    .matches("setup")
                    .count(),
                1,
                "fixture-b setup should still run after the shared root update"
            );
        });
    }

    #[test]
    fn linked_update_switches_clean_worktree_to_default_branch_or_detached_default() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let default_branch = if run_git(&source, &["rev-parse", "--verify", "main"]) {
                "main"
            } else {
                "master"
            };

            assert!(run_git(
                &source,
                &["checkout", "-b", "feature-linked-extension"]
            ));
            assert!(run_git(
                &source,
                &[
                    "push",
                    "--set-upstream",
                    "origin",
                    "feature-linked-extension"
                ]
            ));
            let stable_checkout = home.join("stable-checkout");
            assert!(run_git(
                &source,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    stable_checkout.to_str().expect("stable checkout path"),
                    default_branch,
                ]
            ));

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            let result =
                update("wordpress", false).expect("linked update should use default branch");
            assert!(result.linked);
            assert_eq!(
                result.source_update.old_branch.as_deref(),
                Some("feature-linked-extension")
            );

            let branch_output = Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&source)
                .output()
                .expect("current branch");
            let current_branch = String::from_utf8_lossy(&branch_output.stdout)
                .trim()
                .to_string();
            assert_eq!(
                current_branch,
                result.source_update.new_branch.unwrap_or_default(),
                "linked update metadata should report the resulting branch"
            );
            assert!(
                current_branch == default_branch || current_branch.is_empty(),
                "linked update should use the default branch, or detached origin/default when the branch is checked out in another worktree"
            );
        });
    }

    #[test]
    fn extracted_monorepo_update_reclones_from_stored_source_url() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install extracted extension");
            assert!(!result.path.join(".git").exists());

            write_extension_fixture_with_version(&source, "wordpress", "2.0.0");
            assert!(commit_all(&source, "update extension"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            update("wordpress", false).expect("update extracted extension");

            let updated = load_extension("wordpress").expect("updated extension");
            assert_eq!(updated.version, "2.0.0");
            assert_eq!(
                fs::read_to_string(result.path.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                remote_url.to_string_lossy()
            );
        });
    }

    #[test]
    fn extracted_monorepo_update_keeps_existing_install_when_validation_fails() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install extracted extension");

            fs::write(source.join("wordpress/wordpress.json"), "not json")
                .expect("write invalid manifest");
            assert!(commit_all(&source, "break extension manifest"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            assert!(update("wordpress", false).is_err());

            let current = load_extension("wordpress").expect("previous extension remains loadable");
            assert_eq!(current.version, "1.0.0");
            assert!(
                result.path.join("wordpress.json").exists(),
                "failed update must leave the prior install in place"
            );
        });
    }

    #[test]
    fn copied_extension_manifest_source_metadata_repairs_source_url() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            let extension_dir = extensions_dir.join("rust");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("rust.json"),
                r#"{
  "name": "rust extension",
  "version": "1.0.0",
  "source_url": "https://github.com/Extra-Chill/homeboy-extensions"
}"#,
            )
            .expect("extension manifest");

            let source =
                source_metadata::resolve_source_url("rust").expect("manifest source repair");

            assert_eq!(
                source.url,
                "https://github.com/Extra-Chill/homeboy-extensions"
            );
            let repair = source.repair.expect("repair result");
            assert_eq!(
                repair.source_url,
                "https://github.com/Extra-Chill/homeboy-extensions"
            );
            assert!(repair.reason.contains("manifest sourceUrl"));
            assert_eq!(
                fs::read_to_string(extension_dir.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                "https://github.com/Extra-Chill/homeboy-extensions"
            );
        });
    }

    #[test]
    fn manifest_source_url_alias_repairs_missing_source_url_marker() {
        with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/custom");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("custom.json"),
                r#"{
  "name": "custom extension",
  "version": "1.0.0",
  "sourceUrl": "https://example.com/custom.git"
}"#,
            )
            .expect("extension manifest");

            let source =
                source_metadata::resolve_source_url("custom").expect("manifest source repair");

            assert_eq!(source.url, "https://example.com/custom.git");
            let repair = source.repair.expect("repair result");
            assert!(repair.reason.contains("manifest sourceUrl"));
            assert_eq!(
                fs::read_to_string(extension_dir.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                "https://example.com/custom.git"
            );
        });
    }

    #[test]
    fn copied_unknown_extension_missing_source_metadata_is_actionable_error() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            write_extension_fixture(&extensions_dir, "custom");
            let extension_dir = extensions_dir.join("custom");

            let err = source_metadata::resolve_source_url("custom")
                .expect_err("unknown source stays unresolved");
            let text = err.to_string();
            let hints = err
                .hints
                .iter()
                .map(|hint| hint.message.as_str())
                .collect::<Vec<_>>()
                .join("\n");

            assert!(text.contains("no sourceUrl or .source-url metadata"));
            assert!(hints.contains("homeboy extension install <url> --id custom"));
            assert!(hints.contains(&extension_dir.to_string_lossy().to_string()));
        });
    }

    #[test]
    fn update_all_reports_actionable_missing_source_metadata_skip() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            write_extension_fixture(&extensions_dir, "custom");

            let result = update_all(false);

            assert_eq!(result.skipped, vec!["custom"]);
            assert_eq!(result.skipped_details.len(), 1);
            assert_eq!(result.skipped_details[0].extension_id, "custom");
            assert!(result.skipped_details[0]
                .reason
                .contains("no sourceUrl or .source-url metadata"));
            assert!(result.skipped_details[0]
                .hints
                .iter()
                .any(|hint| hint.contains("homeboy extension install <url> --id custom")));
        });
    }

    #[test]
    fn is_workdir_clean_non_git_dir_returns_true() {
        // Regression test for Extra-Chill/homeboy#1181: tarball / plain-directory
        // installs (no `.git`) must be treated as clean, since there is no
        // working tree to be dirty in the first place.
        let temp = TempDir::new().expect("create tempdir");
        std::fs::write(temp.path().join("some-file.txt"), "content").expect("write file");

        assert!(
            crate::core::git::is_workdir_clean_or_not_git(temp.path()),
            "non-git directory should be treated as clean"
        );
    }

    #[test]
    fn is_workdir_clean_clean_git_repo_returns_true() {
        let temp = TempDir::new().expect("create tempdir");

        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status();
        if init.map(|s| !s.success()).unwrap_or(true) {
            // git not available in this environment; skip.
            return;
        }

        assert!(
            crate::core::git::is_workdir_clean_or_not_git(temp.path()),
            "freshly-initialized git repo with no changes should be clean"
        );
    }

    #[test]
    fn is_workdir_clean_dirty_git_repo_returns_false() {
        let temp = TempDir::new().expect("create tempdir");

        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status();
        if init.map(|s| !s.success()).unwrap_or(true) {
            // git not available in this environment; skip.
            return;
        }

        std::fs::write(temp.path().join("untracked.txt"), "hi").expect("write untracked file");

        assert!(
            !crate::core::git::is_workdir_clean_or_not_git(temp.path()),
            "git repo with untracked file should be reported as dirty"
        );
    }
}
