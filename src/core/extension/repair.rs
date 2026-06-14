use crate::core::config::{self, from_str};
use crate::core::engine::local_files::{self, FileSystem};
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::paths;
use std::path::{Path, PathBuf};

use super::execution::run_setup;
use super::lifecycle::{
    derive_id_from_url, is_git_url, rename_dir, resolve_cloned_extension, slugify_id,
    write_source_metadata,
};
use super::manifest::ExtensionManifest;

const REPLACE_CLONE_TEMP_PREFIX: &str = ".replace-clone-tmp";

#[derive(Debug, Clone)]
pub struct ReplaceResult {
    pub extension_id: String,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    pub source: String,
    pub linked: bool,
    pub source_revision: Option<String>,
}

pub fn replace(source: &str, id_override: Option<&str>) -> Result<ReplaceResult> {
    replace_with_revision(source, id_override, None)
}

pub fn replace_with_revision(
    source: &str,
    id_override: Option<&str>,
    revision: Option<&str>,
) -> Result<ReplaceResult> {
    if is_git_url(source) {
        replace_from_url(source, id_override, revision)
    } else {
        replace_from_path(source, id_override, false)
    }
}

pub fn relink(extension_id: &str, source: &str) -> Result<ReplaceResult> {
    replace_from_path(source, Some(extension_id), true)
}

fn replace_from_url(
    url: &str,
    id_override: Option<&str>,
    revision: Option<&str>,
) -> Result<ReplaceResult> {
    let extension_id = match id_override {
        Some(id) => slugify_id(id)?,
        None => derive_id_from_url(url)?,
    };

    config::check_id_collision(&extension_id, "extension")?;

    let extension_dir = paths::extension(&extension_id)?;
    if !path_exists_or_symlink(&extension_dir) {
        return Err(Error::extension_not_found(extension_id, vec![]));
    }

    local_files::ensure_app_dirs()?;
    let extensions_dir = paths::extensions()?;
    let clone_dir = unique_replace_clone_temp(&extensions_dir, &extension_id)?;
    let staged_dir = extensions_dir.join(format!(".replace-stage-tmp-{}", extension_id));
    let backup_dir = extensions_dir.join(format!(".replace-backup-tmp-{}", extension_id));

    clean_stale_replace_clone_temps(&extensions_dir, &extension_id)?;
    clean_replace_temp(&staged_dir)?;
    clean_replace_temp(&backup_dir)?;

    if let Err(err) = git::clone_repo_at_ref(url, &clone_dir, revision) {
        let _ = clean_replace_temp(&clone_dir);
        return Err(err.with_hint(format!(
            "Homeboy cloned into a fresh replace temp directory and removed it after failure. If git still reports corrupt tmp_pack files, inspect disk/git health and remove stale replace clone temps with: rm -rf {}",
            replace_clone_temp_glob(&extensions_dir, &extension_id)
        )));
    }
    let source_revision = git::short_head_revision(&clone_dir);

    let result = resolve_cloned_extension(&clone_dir, &extension_id, &staged_dir, url);
    if clone_dir.exists() {
        let _ = std::fs::remove_dir_all(&clone_dir);
    }
    result?;

    write_source_metadata(&staged_dir, url, source_revision.clone());

    let old_path = installed_source_path(&extension_dir);
    move_existing_install(&extension_dir, &backup_dir)?;
    if let Err(err) = rename_dir(&staged_dir, &extension_dir) {
        let _ = restore_existing_install(&backup_dir, &extension_dir);
        return Err(err);
    }

    run_setup_or_restore(&extension_id, &extension_dir, &backup_dir)?;
    remove_existing_install(&backup_dir)?;

    Ok(ReplaceResult {
        extension_id,
        old_path,
        new_path: extension_dir,
        source: url.to_string(),
        linked: false,
        source_revision,
    })
}

fn replace_from_path(
    source_path: &str,
    id_override: Option<&str>,
    require_existing_link: bool,
) -> Result<ReplaceResult> {
    let source = resolve_local_source(source_path)?;
    let extension_id = local_extension_id(&source, source_path, id_override)?;
    config::check_id_collision(&extension_id, "extension")?;
    validate_local_extension_source(&source, source_path, &extension_id)?;

    let extension_dir = paths::extension(&extension_id)?;
    if !path_exists_or_symlink(&extension_dir) {
        return Err(Error::extension_not_found(extension_id, vec![]));
    }
    if require_existing_link && !extension_dir.is_symlink() {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!(
                "Extension '{}' is not linked; use 'homeboy extension install --replace <source> --id {}' to replace copied installs.",
                extension_id, extension_id
            ),
            Some(extension_id),
            None,
        ));
    }

    local_files::ensure_app_dirs()?;

    let old_path = installed_source_path(&extension_dir);
    let source_revision = git::short_head_revision(&source);
    let staged_link = extension_dir.with_file_name(format!(".replace-link-tmp-{}", extension_id));
    clean_replace_temp(&staged_link)?;

    create_symlink(&source, &staged_link)?;
    let backup_dir = extension_dir.with_file_name(format!(".replace-backup-tmp-{}", extension_id));
    move_existing_install(&extension_dir, &backup_dir)?;
    if let Err(err) = std::fs::rename(&staged_link, &extension_dir).map_err(|e| {
        Error::internal_io(e.to_string(), Some("replace extension symlink".to_string()))
    }) {
        let _ = restore_existing_install(&backup_dir, &extension_dir);
        return Err(err);
    }

    run_setup_or_restore(&extension_id, &extension_dir, &backup_dir)?;
    remove_existing_install(&backup_dir)?;

    Ok(ReplaceResult {
        extension_id,
        old_path,
        new_path: source.clone(),
        source: source.to_string_lossy().to_string(),
        linked: true,
        source_revision,
    })
}

fn resolve_local_source(source_path: &str) -> Result<PathBuf> {
    let source = Path::new(source_path);
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::internal_io(e.to_string(), Some("get current dir".to_string())))?
            .join(source)
    };

    if !source.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("Path does not exist: {}", source.display()),
            Some(source_path.to_string()),
            None,
        ));
    }

    Ok(source)
}

fn local_extension_id(
    source: &Path,
    source_path: &str,
    id_override: Option<&str>,
) -> Result<String> {
    let dir_name = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source",
            "Could not determine directory name",
            Some(source_path.to_string()),
            None,
        )
    })?;

    match id_override {
        Some(id) => slugify_id(id),
        None => slugify_id(dir_name),
    }
}

fn validate_local_extension_source(
    source: &Path,
    source_path: &str,
    extension_id: &str,
) -> Result<()> {
    let manifest_path = source.join(format!("{}.json", extension_id));
    if !manifest_path.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("No {}.json found at {}", extension_id, source.display()),
            Some(source_path.to_string()),
            None,
        ));
    }

    let manifest_content = local_files::local().read(&manifest_path)?;
    let _manifest: ExtensionManifest = from_str(&manifest_content)?;
    Ok(())
}

fn create_symlink(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(source, target)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    Ok(())
}

fn path_exists_or_symlink(path: &Path) -> bool {
    path.exists() || std::fs::symlink_metadata(path).is_ok()
}

fn installed_source_path(extension_dir: &Path) -> PathBuf {
    std::fs::read_link(extension_dir).unwrap_or_else(|_| extension_dir.to_path_buf())
}

fn move_existing_install(from: &Path, backup: &Path) -> Result<()> {
    if path_exists_or_symlink(backup) {
        remove_existing_install(backup)?;
    }

    if from.is_symlink() {
        std::fs::rename(from, backup).map_err(|e| {
            Error::internal_io(e.to_string(), Some("backup extension symlink".to_string()))
        })?;
    } else {
        rename_dir(from, backup)?;
    }

    Ok(())
}

fn restore_existing_install(backup: &Path, to: &Path) -> Result<()> {
    if !path_exists_or_symlink(backup) {
        return Ok(());
    }

    if backup.is_symlink() {
        std::fs::rename(backup, to).map_err(|e| {
            Error::internal_io(e.to_string(), Some("restore extension symlink".to_string()))
        })?;
    } else {
        rename_dir(backup, to)?;
    }

    Ok(())
}

fn remove_existing_install(path: &Path) -> Result<()> {
    if !path_exists_or_symlink(path) {
        return Ok(());
    }

    if path.is_symlink() || path.is_file() {
        std::fs::remove_file(path).map_err(|e| {
            Error::internal_io(e.to_string(), Some("remove extension path".to_string()))
        })?;
    } else {
        std::fs::remove_dir_all(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("remove extension directory".to_string()),
            )
        })?;
    }

    Ok(())
}

fn run_setup_or_restore(extension_id: &str, extension_dir: &Path, backup_dir: &Path) -> Result<()> {
    if let Err(err) = run_setup(extension_id) {
        let _ = remove_existing_install(extension_dir);
        let _ = restore_existing_install(backup_dir, extension_dir);
        return Err(err);
    }

    Ok(())
}

fn clean_replace_temp(path: &Path) -> Result<()> {
    if path_exists_or_symlink(path) {
        remove_existing_install(path)?;
    }
    Ok(())
}

fn replace_clone_temp_prefix(extension_id: &str) -> String {
    format!("{}-{}", REPLACE_CLONE_TEMP_PREFIX, extension_id)
}

fn replace_clone_temp_glob(extensions_dir: &Path, extension_id: &str) -> String {
    let prefix = replace_clone_temp_prefix(extension_id);
    let legacy = extensions_dir.join(&prefix).to_string_lossy().to_string();
    let unique = extensions_dir
        .join(format!("{}-*", prefix))
        .to_string_lossy()
        .to_string();

    format!("{} {}", legacy, unique)
}

fn is_replace_clone_temp_for_id(file_name: &str, extension_id: &str) -> bool {
    let prefix = replace_clone_temp_prefix(extension_id);
    file_name == prefix || file_name.starts_with(&format!("{}-", prefix))
}

fn unique_replace_clone_temp(extensions_dir: &Path, extension_id: &str) -> Result<PathBuf> {
    let prefix = replace_clone_temp_prefix(extension_id);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    for attempt in 0..100 {
        let path = extensions_dir.join(format!("{}-{}-{}-{}", prefix, pid, nanos, attempt));
        if !path_exists_or_symlink(&path) {
            return Ok(path);
        }
    }

    Err(Error::internal_io(
        "could not allocate unique replace clone temp directory",
        Some(replace_clone_temp_glob(extensions_dir, extension_id)),
    ))
}

fn clean_stale_replace_clone_temps(extensions_dir: &Path, extension_id: &str) -> Result<()> {
    for entry in std::fs::read_dir(extensions_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("read extension directory for stale replace clone temps".to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("read stale replace clone temp entry".to_string()),
            )
        })?;
        let file_name = entry.file_name();
        if is_replace_clone_temp_for_id(&file_name.to_string_lossy(), extension_id) {
            clean_replace_temp(&entry.path())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        clean_stale_replace_clone_temps, relink, replace, replace_with_revision,
        unique_replace_clone_temp,
    };
    use crate::core::extension::{install, load_extension};
    use crate::test_support::with_isolated_home;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

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

    fn write_extension_fixture_with_setup_command(root: &Path, id: &str, setup_command: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "1.0.0",
  "executable": {{
    "runtime": {{
      "setup_command": "{}"
    }}
  }}
}}"#,
                id, setup_command
            ),
        )
        .expect("extension manifest");
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

    #[test]
    fn replace_clone_temp_paths_are_unique_per_run() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            fs::create_dir_all(&extensions_dir).expect("extensions dir");

            let first =
                unique_replace_clone_temp(&extensions_dir, "swift").expect("first temp clone path");
            fs::create_dir_all(&first).expect("reserve first temp clone path");
            let second = unique_replace_clone_temp(&extensions_dir, "swift")
                .expect("second temp clone path");

            assert_ne!(first, second);
            assert!(first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".replace-clone-tmp-swift-"));
            assert!(second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".replace-clone-tmp-swift-"));
        });
    }

    #[test]
    fn replace_cleans_stale_clone_temp_dirs_before_clone() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            fs::create_dir_all(&extensions_dir).expect("extensions dir");
            let stale_legacy = extensions_dir.join(".replace-clone-tmp-swift");
            let stale_unique = extensions_dir.join(".replace-clone-tmp-swift-123-456-0");
            let unrelated = extensions_dir.join(".replace-clone-tmp-rust");
            let similarly_prefixed = extensions_dir.join(".replace-clone-tmp-swiftly");
            fs::create_dir_all(stale_legacy.join(".git/objects/pack")).expect("stale legacy temp");
            fs::write(
                stale_legacy.join(".git/objects/pack/tmp_pack_stale"),
                "partial pack",
            )
            .expect("stale pack");
            fs::create_dir_all(&stale_unique).expect("stale unique temp");
            fs::create_dir_all(&unrelated).expect("unrelated temp");
            fs::create_dir_all(&similarly_prefixed).expect("similarly prefixed temp");

            clean_stale_replace_clone_temps(&extensions_dir, "swift")
                .expect("clean stale clone temps");

            assert!(!stale_legacy.exists());
            assert!(!stale_unique.exists());
            assert!(unrelated.exists());
            assert!(similarly_prefixed.exists());
        });
    }

    #[test]
    fn relink_replaces_existing_symlink_source() {
        with_isolated_home(|home| {
            let home = home.path();
            let old_source = home.join("old-source");
            let new_source = home.join("new-source");
            write_extension_fixture(&old_source, "swift");
            write_extension_fixture_with_version(&new_source, "swift", "2.0.0");

            install(&old_source.join("swift").to_string_lossy(), Some("swift"))
                .expect("install linked extension");

            let result = relink("swift", &new_source.join("swift").to_string_lossy())
                .expect("relink should replace symlink");

            let installed_path = home.join(".config/homeboy/extensions/swift");
            assert!(installed_path.is_symlink());
            assert_eq!(result.extension_id, "swift");
            assert_eq!(result.old_path, old_source.join("swift"));
            assert_eq!(result.new_path, new_source.join("swift"));
            assert!(result.linked);
            assert_eq!(
                fs::read_link(installed_path).expect("read replacement link"),
                new_source.join("swift")
            );

            let extension = load_extension("swift").expect("load relinked extension");
            assert_eq!(extension.version, "2.0.0");
        });
    }

    #[test]
    fn relink_fails_and_restores_existing_link_when_setup_fails() {
        with_isolated_home(|home| {
            let home = home.path();
            let old_source = home.join("old-source");
            let new_source = home.join("new-source");
            write_extension_fixture(&old_source, "swift");
            write_extension_fixture_with_setup_command(
                &new_source,
                "swift",
                "bash {{extension_path}}/scripts/build/setup.sh",
            );

            install(&old_source.join("swift").to_string_lossy(), Some("swift"))
                .expect("install linked extension");

            let err = relink("swift", &new_source.join("swift").to_string_lossy())
                .expect_err("relink should fail when setup fails");

            assert_eq!(err.message, "IO error");
            assert!(err.details.to_string().contains("Setup command failed"));
            let installed_path = home.join(".config/homeboy/extensions/swift");
            assert!(installed_path.is_symlink());
            assert_eq!(
                fs::read_link(installed_path).expect("read restored link"),
                old_source.join("swift")
            );
        });
    }

    #[test]
    fn replace_updates_copied_extension_from_git_source() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "swift") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let install_result = install(&remote_url.to_string_lossy(), Some("swift"))
                .expect("install copied extension");
            assert!(!install_result.path.is_symlink());

            write_extension_fixture_with_version(&source, "swift", "2.0.0");
            assert!(commit_all(&source, "update extension"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            let result = replace(&remote_url.to_string_lossy(), Some("swift"))
                .expect("replace copied extension");

            assert_eq!(result.extension_id, "swift");
            assert_eq!(result.old_path, install_result.path);
            assert_eq!(
                result.new_path,
                home.join(".config/homeboy/extensions/swift")
            );
            assert!(!result.linked);
            assert!(result.source_revision.is_some());

            let extension = load_extension("swift").expect("load replaced extension");
            assert_eq!(extension.version, "2.0.0");
        });
    }

    #[test]
    fn test_replace_with_revision() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "swift") {
                Some(remote) => remote,
                None => return,
            };
            let pinned_revision = match git_output(&source, &["rev-parse", "--short", "HEAD"]) {
                Some(revision) => revision,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("swift"))
                .expect("install copied extension");

            write_extension_fixture_with_version(&source, "swift", "2.0.0");
            assert!(commit_all(&source, "update extension"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            let result = replace_with_revision(
                &remote_url.to_string_lossy(),
                Some("swift"),
                Some(&pinned_revision),
            )
            .expect("replace copied extension at pinned revision");

            let extension = load_extension("swift").expect("load replaced extension");
            assert_eq!(extension.version, "1.0.0");
            assert_eq!(
                result.source_revision.as_deref(),
                Some(pinned_revision.as_str())
            );
        });
    }
}
