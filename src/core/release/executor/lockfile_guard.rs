//! Guard against non-deterministic isolated builds caused by a missing
//! committed lockfile alongside git-pinned dependencies.
//!
//! ## The failure this prevents
//!
//! Homeboy's release/deploy builds run in an isolated tree that only contains
//! committed files. When a manifest pins a dependency to a git ref (e.g.
//! `github:org/pkg#v1.2.3`) but the corresponding lockfile is gitignored or
//! never committed, the isolated install re-resolves that ref from scratch.
//! Git refs are mutable and the package manager's git cache can hand back a
//! *stale* commit for the same tag, producing a different — and sometimes
//! broken — artifact than the one the developer built locally. The build still
//! "succeeds", so a partial/incorrect artifact ships with no hard signal.
//!
//! The durable fix is to commit the lockfile so the isolated build resolves the
//! exact same tree every time. This guard detects the dangerous shape — a
//! manifest with git-pinned deps but no committed lockfile — and fails the
//! release preflight loudly before any artifact is built or shipped.
//!
//! Ecosystem specifics (npm manifest/lockfile names, git-ref dependency syntax)
//! are intentionally contained in this module rather than leaking into deep
//! core; the release pipeline simply invokes the guard during preflight.

use std::path::{Component as PathComponent, Path, PathBuf};

use crate::core::error::{Error, Result};
use crate::core::git;

/// Manifest / lockfile pairing for a single package ecosystem. The first
/// existing-and-committed lockfile in `lockfiles` satisfies the guard.
struct ManifestSpec {
    manifest: &'static str,
    lockfiles: &'static [&'static str],
}

/// Supported manifest ecosystems. Only npm-family manifests currently express
/// git-pinned dependencies in the shape this guard inspects; the list is a
/// table so additional ecosystems can be added without touching the walk.
const MANIFEST_SPECS: &[ManifestSpec] = &[ManifestSpec {
    manifest: "package.json",
    lockfiles: &[
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
    ],
}];

/// Directories that never contain source-of-truth manifests for the component
/// being released, and would make the walk slow or noisy.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "vendor", "target", "dist", "build"];

/// A manifest that pins git dependencies but has no committed lockfile.
struct UnlockedManifest {
    /// Manifest path relative to the component root (display form).
    manifest_rel: String,
    /// Example git-pinned dependency specs (for the operator-facing message).
    pinned: Vec<String>,
    /// Lockfile names that would satisfy the guard for this manifest.
    expected_lockfiles: Vec<String>,
}

/// Inspect the component tree for manifests that pin git dependencies without a
/// committed lockfile, and fail loudly when any are found.
///
/// `component_root` must be the original component checkout (it has a `.git`
/// directory) so committed/tracked state can be determined. When the component
/// is not a git repository the guard cannot reason about committed state and is
/// skipped (the broader release flow handles non-git components elsewhere).
pub(crate) fn guard_committed_lockfiles(component_root: &Path) -> Result<()> {
    let root_str = component_root.to_string_lossy();
    if !git::is_git_repo(&root_str) {
        return Ok(());
    }

    let mut offenders = Vec::new();
    collect_unlocked_manifests(component_root, component_root, &mut offenders);

    if offenders.is_empty() {
        return Ok(());
    }

    Err(missing_lockfile_error(&offenders))
}

/// Inspect npm-family manifests for `file:` dependencies that cannot be
/// reproduced from the component checkout used by release packaging.
pub(crate) fn guard_local_file_dependencies(component_root: &Path) -> Result<()> {
    let mut offenders = Vec::new();
    collect_local_file_dependency_offenders(component_root, component_root, &mut offenders);

    if offenders.is_empty() {
        return Ok(());
    }

    Err(local_file_dependency_error(&offenders))
}

fn collect_unlocked_manifests(
    component_root: &Path,
    dir: &Path,
    offenders: &mut Vec<UnlockedManifest>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_unlocked_manifests(component_root, &path, offenders);
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Some(spec) = MANIFEST_SPECS
            .iter()
            .find(|spec| spec.manifest == file_name.as_ref())
        else {
            continue;
        };

        if let Some(offender) = inspect_manifest(component_root, &path, spec) {
            offenders.push(offender);
        }
    }
}

fn collect_local_file_dependency_offenders(
    component_root: &Path,
    dir: &Path,
    offenders: &mut Vec<LocalFileDependency>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_local_file_dependency_offenders(component_root, &path, offenders);
            continue;
        }

        if !file_type.is_file() || entry.file_name() != std::ffi::OsStr::new("package.json") {
            continue;
        }

        offenders.extend(local_file_dependency_offenders(component_root, &path));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LocalFileDependency {
    manifest_rel: String,
    package: String,
    spec: String,
    resolved_path: String,
    problem: LocalFileDependencyProblem,
}

#[derive(Debug, PartialEq, Eq)]
enum LocalFileDependencyProblem {
    Missing,
    OutsideCheckout,
}

fn local_file_dependency_offenders(
    component_root: &Path,
    manifest_path: &Path,
) -> Vec<LocalFileDependency> {
    let contents = match std::fs::read_to_string(manifest_path) {
        Ok(contents) => contents,
        Err(_) => return Vec::new(),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };

    let manifest_dir = manifest_path.parent().unwrap_or(component_root);
    let manifest_rel = manifest_path
        .strip_prefix(component_root)
        .map(|rel| rel.to_string_lossy().to_string())
        .unwrap_or_else(|_| manifest_path.to_string_lossy().to_string());
    let sections = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ];

    let mut offenders = Vec::new();
    for section in sections {
        let Some(map) = value.get(section).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (package, spec) in map {
            let Some(spec) = spec.as_str() else { continue };
            let Some(local_path) = spec.strip_prefix("file:") else {
                continue;
            };
            let resolved = resolve_file_dependency_path(manifest_dir, local_path);
            let problem = if !resolved.exists() {
                Some(LocalFileDependencyProblem::Missing)
            } else if !path_is_inside(component_root, &resolved) {
                Some(LocalFileDependencyProblem::OutsideCheckout)
            } else {
                None
            };

            if let Some(problem) = problem {
                offenders.push(LocalFileDependency {
                    manifest_rel: manifest_rel.clone(),
                    package: package.clone(),
                    spec: spec.to_string(),
                    resolved_path: resolved.display().to_string(),
                    problem,
                });
            }
        }
    }

    offenders.sort_by(|a, b| {
        (&a.manifest_rel, &a.package, &a.spec).cmp(&(&b.manifest_rel, &b.package, &b.spec))
    });
    offenders
}

fn resolve_file_dependency_path(manifest_dir: &Path, local_path: &str) -> PathBuf {
    let path = Path::new(local_path);
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&manifest_dir.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            PathComponent::CurDir => {}
            PathComponent::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn path_is_inside(root: &Path, path: &Path) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| normalize_path(root));
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path));
    path == root || path.starts_with(root)
}

/// Returns `Some` when `manifest_path` pins git dependencies but has no
/// committed lockfile sibling.
fn inspect_manifest(
    component_root: &Path,
    manifest_path: &Path,
    spec: &ManifestSpec,
) -> Option<UnlockedManifest> {
    let contents = std::fs::read_to_string(manifest_path).ok()?;
    let pinned = git_pinned_dependencies(&contents);
    if pinned.is_empty() {
        return None;
    }

    let dir = manifest_path.parent().unwrap_or(component_root);
    let has_committed_lockfile = spec.lockfiles.iter().any(|lockfile| {
        let lockfile_path = dir.join(lockfile);
        if !lockfile_path.exists() {
            return false;
        }
        match lockfile_path.strip_prefix(component_root) {
            Ok(rel) => git::is_tracked_path(component_root, &rel.to_string_lossy()),
            Err(_) => false,
        }
    });

    if has_committed_lockfile {
        return None;
    }

    let manifest_rel = manifest_path
        .strip_prefix(component_root)
        .map(|rel| rel.to_string_lossy().to_string())
        .unwrap_or_else(|_| manifest_path.to_string_lossy().to_string());

    Some(UnlockedManifest {
        manifest_rel,
        pinned,
        expected_lockfiles: spec.lockfiles.iter().map(|l| l.to_string()).collect(),
    })
}

/// Extract dependency specs that resolve from a mutable git ref. Parses the
/// manifest's `dependencies`, `devDependencies`, and `optionalDependencies`
/// maps and returns the `name@spec` pairs whose value points at a git source.
fn git_pinned_dependencies(manifest_contents: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(manifest_contents) else {
        return Vec::new();
    };

    let sections = ["dependencies", "devDependencies", "optionalDependencies"];

    let mut pinned = Vec::new();
    for section in sections {
        let Some(map) = value.get(section).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (name, spec) in map {
            let Some(spec) = spec.as_str() else { continue };
            if is_git_pinned_spec(spec) {
                pinned.push(format!("{name}@{spec}"));
            }
        }
    }

    pinned.sort();
    pinned.dedup();
    pinned
}

/// Detect dependency specs that resolve from a git source rather than a content
/// -addressed registry tarball. These are the specs that re-resolve (and can go
/// stale) without a committed lockfile.
fn is_git_pinned_spec(spec: &str) -> bool {
    let spec = spec.trim();

    // Explicit git protocols / hosting shorthands understood by npm-family
    // package managers.
    const GIT_PREFIXES: &[&str] = &[
        "git+",
        "git://",
        "github:",
        "gitlab:",
        "bitbucket:",
        "gist:",
    ];
    if GIT_PREFIXES.iter().any(|prefix| spec.starts_with(prefix)) {
        return true;
    }

    // `user/repo` or `user/repo#ref` shorthand resolves to GitHub. Exclude
    // registry ranges and file/link/workspace specs, which never contain a
    // bare `owner/repo` slug.
    if spec.contains("://") || spec.starts_with("file:") || spec.starts_with("link:") {
        return false;
    }
    if spec.starts_with("npm:")
        || spec.starts_with("workspace:")
        || spec.starts_with('^')
        || spec.starts_with('~')
        || spec.starts_with('>')
        || spec.starts_with('<')
        || spec.starts_with('=')
        || spec.starts_with('*')
    {
        return false;
    }

    // `owner/repo` shorthand: exactly one slash, both halves non-empty, and the
    // value is not a semver-looking string.
    let slug = spec.split('#').next().unwrap_or(spec);
    let parts: Vec<&str> = slug.split('/').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && !parts[0].chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn missing_lockfile_error(offenders: &[UnlockedManifest]) -> Error {
    let mut lines = vec![
        "Release blocked: git-pinned dependencies without a committed lockfile.".to_string(),
        String::new(),
        "The isolated release/deploy build only sees committed files. A manifest \
         that pins a git ref (e.g. github:org/pkg#tag) with no committed lockfile \
         re-resolves that ref every build and can silently pull a stale cached \
         commit — shipping a different (possibly broken) artifact than you built \
         locally."
            .to_string(),
        String::new(),
    ];

    for offender in offenders {
        lines.push(format!("  {}", offender.manifest_rel));
        for dep in &offender.pinned {
            lines.push(format!("    pinned: {dep}"));
        }
        lines.push(format!(
            "    missing committed lockfile (one of: {})",
            offender.expected_lockfiles.join(", ")
        ));
    }

    let hints = vec![
        "Generate and commit the lockfile (e.g. `npm install` then commit \
         package-lock.json) so the isolated build resolves the exact same tree."
            .to_string(),
        "Ensure the lockfile is not gitignored — `git check-ignore <lockfile>` \
         should report nothing."
            .to_string(),
    ];

    Error::validation_invalid_argument(
        "release.preflight.lockfile",
        lines.join("\n"),
        None,
        Some(hints),
    )
}

fn local_file_dependency_error(offenders: &[LocalFileDependency]) -> Error {
    let mut lines = vec![
        "Release blocked: package.json file: dependencies must be reproducible from the component checkout.".to_string(),
        String::new(),
        "Release packaging runs from an isolated component copy. Local file dependencies that point outside the component checkout are non-reproducible, and missing file dependencies fail later at install/build time.".to_string(),
        String::new(),
    ];

    for offender in offenders {
        let problem = match offender.problem {
            LocalFileDependencyProblem::Missing => "missing target",
            LocalFileDependencyProblem::OutsideCheckout => "outside component checkout",
        };
        lines.push(format!(
            "  {}: {}@{} -> {} ({})",
            offender.manifest_rel, offender.package, offender.spec, offender.resolved_path, problem
        ));
    }

    Error::validation_invalid_argument(
        "release.preflight.local_file_dependencies",
        lines.join("\n"),
        None,
        Some(vec![
            "Use a published package version for releaseable dependencies.".to_string(),
            "If this is an intentional workspace release, configure the dependency through the workspace release path instead of a checkout-external file: path.".to_string(),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "--quiet"]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "Test"]);
    }

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    const GIT_PINNED_MANIFEST: &str = r#"{
        "name": "theme",
        "dependencies": {
            "@org/tokens": "github:org/tokens#v0.8.1",
            "lodash": "^4.17.0"
        }
    }"#;

    #[test]
    fn detects_git_shorthand_and_protocol_specs() {
        assert!(is_git_pinned_spec("github:org/tokens#v0.8.1"));
        assert!(is_git_pinned_spec("git+https://github.com/org/pkg.git"));
        assert!(is_git_pinned_spec("git://github.com/org/pkg.git"));
        assert!(is_git_pinned_spec("gitlab:org/pkg"));
        assert!(is_git_pinned_spec("org/tokens#v0.8.1"));
        assert!(is_git_pinned_spec("org/tokens"));
    }

    #[test]
    fn ignores_registry_and_local_specs() {
        assert!(!is_git_pinned_spec("^4.17.0"));
        assert!(!is_git_pinned_spec("~1.2.3"));
        assert!(!is_git_pinned_spec("1.2.3"));
        assert!(!is_git_pinned_spec("*"));
        assert!(!is_git_pinned_spec(">=2.0.0"));
        assert!(!is_git_pinned_spec("npm:@scope/pkg@1.0.0"));
        assert!(!is_git_pinned_spec("file:../local"));
        assert!(!is_git_pinned_spec("link:../local"));
        assert!(!is_git_pinned_spec("workspace:*"));
        assert!(!is_git_pinned_spec("https://example.com/pkg.tgz"));
    }

    #[test]
    fn parses_pinned_deps_from_all_sections() {
        let manifest = r#"{
            "dependencies": { "a": "github:org/a#v1" },
            "devDependencies": { "b": "org/b#v2", "c": "^1.0.0" },
            "optionalDependencies": { "d": "git+https://x/d.git" }
        }"#;
        let pinned = git_pinned_dependencies(manifest);
        assert_eq!(
            pinned,
            vec![
                "a@github:org/a#v1".to_string(),
                "b@org/b#v2".to_string(),
                "d@git+https://x/d.git".to_string(),
            ]
        );
    }

    #[test]
    fn fails_when_git_pinned_dep_has_no_committed_lockfile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        init_repo(root);
        write(root, "package.json", GIT_PINNED_MANIFEST);
        run_git(root, &["add", "package.json"]);
        run_git(root, &["commit", "--quiet", "-m", "init"]);

        let err = guard_committed_lockfiles(root).expect_err("should block release");
        assert!(err.message.contains("git-pinned dependencies"));
        assert!(err.message.contains("package.json"));
        assert!(err.message.contains("github:org/tokens#v0.8.1"));
    }

    #[test]
    fn fails_when_lockfile_present_but_gitignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        init_repo(root);
        write(root, "package.json", GIT_PINNED_MANIFEST);
        write(root, "package-lock.json", "{}");
        write(root, ".gitignore", "package-lock.json\n");
        run_git(root, &["add", "package.json", ".gitignore"]);
        run_git(root, &["commit", "--quiet", "-m", "init"]);

        let err = guard_committed_lockfiles(root)
            .expect_err("gitignored lockfile should not satisfy the guard");
        assert!(err.message.contains("git-pinned dependencies"));
    }

    #[test]
    fn passes_when_lockfile_committed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        init_repo(root);
        write(root, "package.json", GIT_PINNED_MANIFEST);
        write(root, "package-lock.json", "{}");
        run_git(root, &["add", "package.json", "package-lock.json"]);
        run_git(root, &["commit", "--quiet", "-m", "init"]);

        guard_committed_lockfiles(root).expect("committed lockfile satisfies the guard");
    }

    #[test]
    fn passes_when_no_git_pinned_deps() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        init_repo(root);
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "lodash": "^4.17.0" } }"#,
        );
        run_git(root, &["add", "package.json"]);
        run_git(root, &["commit", "--quiet", "-m", "init"]);

        guard_committed_lockfiles(root).expect("registry-only deps need no lockfile guard");
    }

    #[test]
    fn local_file_dependency_guard_blocks_missing_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "@org/ui": "file:../missing-ui" } }"#,
        );

        let err = guard_local_file_dependencies(root).expect_err("missing file dep blocks release");

        assert!(err.message.contains("file: dependencies"));
        assert!(err.message.contains("@org/ui@file:../missing-ui"));
        assert!(err.message.contains("missing target"));
        assert!(err.message.contains("../missing-ui"));
    }

    #[test]
    fn local_file_dependency_guard_blocks_existing_targets_outside_checkout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("component");
        let sibling = temp.path().join("agenttic/packages/agenttic-ui");
        std::fs::create_dir_all(&root).expect("component dir");
        std::fs::create_dir_all(&sibling).expect("sibling dir");
        write(
            &root,
            "package.json",
            r#"{ "dependencies": { "@automattic/agenttic-ui": "file:../agenttic/packages/agenttic-ui" } }"#,
        );

        let err = guard_local_file_dependencies(&root)
            .expect_err("checkout-external file dep blocks release");

        assert!(err
            .message
            .contains("@automattic/agenttic-ui@file:../agenttic/packages/agenttic-ui"));
        assert!(err.message.contains("outside component checkout"));
        assert!(err.message.contains("published package version"));
        assert!(err.message.contains("workspace release path"));
    }

    #[test]
    fn local_file_dependency_guard_allows_existing_targets_inside_checkout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::create_dir_all(root.join("packages/ui")).expect("local package dir");
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "@org/ui": "file:./packages/ui" } }"#,
        );

        guard_local_file_dependencies(root).expect("in-checkout file dep is reproducible");
    }

    #[test]
    fn skips_node_modules_and_non_git_trees() {
        // Non-git tree: guard is a no-op.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "package.json", GIT_PINNED_MANIFEST);
        guard_committed_lockfiles(root).expect("non-git tree is skipped");

        // node_modules manifests are not inspected.
        let temp2 = tempfile::tempdir().expect("tempdir");
        let root2 = temp2.path();
        init_repo(root2);
        write(root2, "node_modules/dep/package.json", GIT_PINNED_MANIFEST);
        write(root2, "package.json", r#"{ "name": "root" }"#);
        run_git(root2, &["add", "."]);
        run_git(root2, &["commit", "--quiet", "-m", "init"]);
        guard_committed_lockfiles(root2).expect("node_modules is skipped");
    }
}
