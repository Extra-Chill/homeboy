//! Extension update-check + source-URL utilities (core glue over an extension's
//! git checkout). Relocated from the extension lifecycle module - depends only on
//! core paths/git/error + the core extension store, no extension behavior.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::extension_store::{is_extension_linked, load_extension};
use crate::git;
use crate::paths;

/// Check if a string looks like a git URL (vs a local path).
pub fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
}

/// Check if a git-cloned extension has updates available.
/// Runs `git fetch` then checks if HEAD is behind the remote tracking branch.
/// Returns None for linked extensions or if check fails.
pub fn check_update_available(extension_id: &str) -> Option<UpdateAvailable> {
    let extension_dir = paths::extension(extension_id).ok()?;
    if !extension_dir.exists() || is_extension_linked(extension_id) {
        return None;
    }

    // Check it's a git repo
    if !extension_dir.join(".git").exists() {
        return None;
    }

    // Fetch latest (best-effort, short timeout)
    Command::new("git")
        .args(["fetch", "--quiet"])
        .current_dir(&extension_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;

    // Check how many commits we're behind
    let output = Command::new("git")
        .args(["rev-list", "HEAD..@{u}", "--count"])
        .current_dir(&extension_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;

    let count_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let behind_count: usize = count_str.parse().ok()?;

    if behind_count == 0 {
        return None;
    }

    // Get installed version
    let extension = load_extension(extension_id).ok()?;
    let installed_version = extension.version.clone();

    Some(UpdateAvailable {
        extension_id: extension_id.to_string(),
        installed_version,
        behind_count,
    })
}
#[derive(Debug, Clone)]
pub struct UpdateAvailable {
    pub extension_id: String,
    pub installed_version: String,
    pub behind_count: usize,
}

pub fn read_source_revision(extension_id: &str) -> Option<String> {
    let extension_dir = paths::extension(extension_id).ok()?;
    read_source_revision_at(&extension_dir)
}

/// Read provenance for an extension at an explicit path. This supports callers
/// that materialize a selected extension outside the ambient extension store.
pub fn read_source_revision_at(extension_dir: &Path) -> Option<String> {
    if !extension_dir.exists() {
        return None;
    }

    // Try .git first (single-extension repos and linked extensions)
    if let Some(rev) = git::head_sha(extension_dir) {
        return Some(rev);
    }

    // Fall back to source metadata files (monorepo installs and staged linked installs).
    read_source_metadata_value(extension_dir, "revision")
}

/// Files Homeboy generates inside an installed extension directory. They are
/// install-local provenance, not extension source, so they never make a
/// checkout dirty for parity purposes.
const GENERATED_SOURCE_METADATA: [&str; 3] =
    [".source-url", ".source-revision", ".source-requested-ref"];

/// Whether a committed revision still describes the bytes on disk.
///
/// A git SHA is only a faithful stand-in for extension content while the
/// checkout is clean. Once it is dirty the SHA describes what was *committed*,
/// not what is *there*, which is why revision-only parity silently accepts an
/// edited-but-uncommitted extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionSourceCleanliness {
    /// Git checkout with no uncommitted, in-scope changes: the revision is a
    /// sound content identity.
    Clean,
    /// Git checkout with uncommitted changes: the revision is NOT a content
    /// identity. `changed_paths` is repo-root-relative and capped for display.
    Dirty { changed_paths: Vec<String> },
    /// Not a git checkout, or git could not answer. Cleanliness is unknown, so
    /// the revision is provenance only and local edits cannot be detected.
    Unknown,
}

/// Maximum number of dirty paths carried into a parity diagnostic. Enough to
/// identify what changed without pasting an entire working tree into an error.
const MAX_REPORTED_DIRTY_PATHS: usize = 10;

pub fn read_source_cleanliness(extension_id: &str) -> ExtensionSourceCleanliness {
    let Ok(extension_dir) = paths::extension(extension_id) else {
        return ExtensionSourceCleanliness::Unknown;
    };
    read_source_cleanliness_at(&extension_dir)
}

/// Classify the extension checkout at an explicit path.
///
/// Scoped to the extension directory itself: a monorepo of extensions must not
/// report `rust` as dirty because `nodejs` was edited.
pub fn read_source_cleanliness_at(extension_dir: &Path) -> ExtensionSourceCleanliness {
    if !extension_dir.exists() {
        return ExtensionSourceCleanliness::Unknown;
    }

    // The directory must actually be versioned by the repository that contains
    // it. A stray enclosing repository — a home directory under version
    // control, a temp root inside a checkout — would otherwise report every
    // untracked extension file as an uncommitted edit. Nothing tracked here
    // means the enclosing repository does not describe this extension, so its
    // revision says nothing about these bytes either.
    if git::output_optional(extension_dir, &["ls-files", "--", "."]).is_none() {
        return ExtensionSourceCleanliness::Unknown;
    }

    let Some(status) = git::status_porcelain_scoped(extension_dir) else {
        return ExtensionSourceCleanliness::Unknown;
    };

    let changed_paths = status
        .lines()
        .filter_map(dirty_path_from_status_line)
        .filter(|path| !is_generated_source_metadata_path(path))
        .take(MAX_REPORTED_DIRTY_PATHS)
        .collect::<Vec<_>>();

    if changed_paths.is_empty() {
        ExtensionSourceCleanliness::Clean
    } else {
        ExtensionSourceCleanliness::Dirty { changed_paths }
    }
}

fn dirty_path_from_status_line(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    // Renames report `old -> new`; the new path is the one that exists now.
    let path = path
        .rsplit_once(" -> ")
        .map(|(_, new_path)| new_path)
        .unwrap_or(path);
    Some(path.trim_matches('"').replace('\\', "/"))
}

fn is_generated_source_metadata_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    GENERATED_SOURCE_METADATA.contains(&name)
}

pub fn read_source_metadata_value(extension_dir: &Path, kind: &str) -> Option<String> {
    let sidecar =
        source_metadata_dir(extension_dir).join(source_metadata_file(extension_dir, kind));
    let embedded = extension_dir.join(format!(".source-{kind}"));
    let paths = if extension_dir.is_symlink() {
        [sidecar, embedded]
    } else {
        [embedded, sidecar]
    };

    for path in paths {
        if let Some(value) = std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Some(value);
        }
    }

    None
}

pub fn read_source_url(extension_dir: &Path) -> Option<String> {
    read_source_metadata_value(extension_dir, "url")
}

pub fn source_metadata_dir(extension_dir: &Path) -> PathBuf {
    if extension_dir.is_symlink() {
        return extension_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
    }

    extension_dir.to_path_buf()
}

fn source_metadata_file(extension_dir: &std::path::Path, kind: &str) -> String {
    if extension_dir.is_symlink() {
        if let Some(name) = extension_dir.file_name().and_then(|name| name.to_str()) {
            return format!(".{name}.source-{kind}");
        }
    }

    format!(".source-{kind}")
}

#[cfg(test)]
mod cleanliness_tests {
    use super::*;

    #[test]
    fn modified_and_untracked_status_lines_yield_paths() {
        assert_eq!(
            dirty_path_from_status_line(" M rust/lint.sh").as_deref(),
            Some("rust/lint.sh")
        );
        assert_eq!(
            dirty_path_from_status_line("?? rust/new-step.sh").as_deref(),
            Some("rust/new-step.sh")
        );
        assert_eq!(
            dirty_path_from_status_line("M  rust/lint.sh").as_deref(),
            Some("rust/lint.sh")
        );
    }

    #[test]
    fn rename_status_lines_report_the_current_path() {
        assert_eq!(
            dirty_path_from_status_line("R  rust/old.sh -> rust/new.sh").as_deref(),
            Some("rust/new.sh")
        );
    }

    #[test]
    fn generated_install_metadata_is_not_extension_source() {
        assert!(is_generated_source_metadata_path(".source-url"));
        assert!(is_generated_source_metadata_path("rust/.source-revision"));
        assert!(is_generated_source_metadata_path(
            "rust/.source-requested-ref"
        ));
        assert!(!is_generated_source_metadata_path("rust/lint.sh"));
        assert!(!is_generated_source_metadata_path("rust/source-url.md"));
    }

    #[test]
    fn a_missing_extension_directory_is_unknown_not_clean() {
        let missing = Path::new("/nonexistent/homeboy/extension/for/cleanliness");

        assert_eq!(
            read_source_cleanliness_at(missing),
            ExtensionSourceCleanliness::Unknown
        );
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_all(path: &Path) {
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);
    }

    #[test]
    fn a_non_git_extension_directory_is_unknown_not_clean() {
        let dir = tempfile::tempdir().expect("extension dir");
        std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");

        assert_eq!(
            read_source_cleanliness_at(dir.path()),
            ExtensionSourceCleanliness::Unknown
        );
    }

    #[test]
    fn a_committed_extension_checkout_is_clean() {
        let dir = tempfile::tempdir().expect("extension dir");
        std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        commit_all(dir.path());

        assert_eq!(
            read_source_cleanliness_at(dir.path()),
            ExtensionSourceCleanliness::Clean
        );
    }

    #[test]
    fn an_edited_but_uncommitted_extension_checkout_is_dirty() {
        let dir = tempfile::tempdir().expect("extension dir");
        std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        std::fs::write(dir.path().join("lint.sh"), "echo committed\n").expect("step");
        commit_all(dir.path());
        std::fs::write(dir.path().join("lint.sh"), "echo edited\n").expect("edit");

        assert_eq!(
            read_source_cleanliness_at(dir.path()),
            ExtensionSourceCleanliness::Dirty {
                changed_paths: vec!["lint.sh".to_string()]
            }
        );
    }

    #[test]
    fn generated_install_metadata_does_not_make_a_checkout_dirty() {
        let dir = tempfile::tempdir().expect("extension dir");
        std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        commit_all(dir.path());
        std::fs::write(dir.path().join(".source-url"), "https://example.test/rust")
            .expect("source url");
        std::fs::write(dir.path().join(".source-revision"), "abc123").expect("source revision");
        std::fs::write(dir.path().join(".source-requested-ref"), "main").expect("requested ref");

        assert_eq!(
            read_source_cleanliness_at(dir.path()),
            ExtensionSourceCleanliness::Clean
        );
    }

    /// A repository that merely encloses the extension directory without
    /// tracking any of it does not describe those bytes, so cleanliness is
    /// unknown rather than dirty. Without this, an untracked extension under a
    /// version-controlled home would fail parity on every dispatch.
    #[test]
    fn an_untracked_directory_inside_an_enclosing_repo_is_unknown_not_dirty() {
        let repo = tempfile::tempdir().expect("enclosing repo");
        std::fs::write(repo.path().join("README.md"), "enclosing\n").expect("readme");
        commit_all(repo.path());
        let extension_dir = repo.path().join("untracked/extensions/rust");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(extension_dir.join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");

        assert_eq!(
            read_source_cleanliness_at(&extension_dir),
            ExtensionSourceCleanliness::Unknown
        );
    }

    #[test]
    fn a_dirty_sibling_extension_does_not_make_this_extension_dirty() {
        let repo = tempfile::tempdir().expect("monorepo");
        let rust = repo.path().join("rust");
        let nodejs = repo.path().join("nodejs");
        std::fs::create_dir_all(&rust).expect("rust dir");
        std::fs::create_dir_all(&nodejs).expect("nodejs dir");
        std::fs::write(rust.join("lint.sh"), "echo rust\n").expect("rust step");
        std::fs::write(nodejs.join("lint.sh"), "echo nodejs\n").expect("nodejs step");
        commit_all(repo.path());
        std::fs::write(nodejs.join("lint.sh"), "echo edited\n").expect("edit sibling");

        assert_eq!(
            read_source_cleanliness_at(&rust),
            ExtensionSourceCleanliness::Clean
        );
        assert_eq!(
            read_source_cleanliness_at(&nodejs),
            ExtensionSourceCleanliness::Dirty {
                changed_paths: vec!["nodejs/lint.sh".to_string()]
            }
        );
    }
}
