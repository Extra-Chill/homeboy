use crate::core::component::{Component, VersionTarget};
use crate::core::release::version;

#[derive(Debug, serde::Serialize)]
pub(crate) struct VersionTargetMismatch {
    pub(crate) file: String,
    pub(crate) expected: String,
    pub(crate) found: Option<String>,
}

/// Re-read every version target from disk and return any that don't show
/// `expected_version`. Returns `None` when every target matches (the success
/// case). Returns `Some(non_empty_vec)` when at least one target failed to
/// update — caller treats that as a failed bump.
///
/// This is a defense-in-depth check around `bump_component_version`: if any
/// upstream change ever causes the function to return Ok without actually
/// writing every target, this catches it before `state.version` advances.
pub(crate) fn collect_version_target_mismatches(
    component: &Component,
    expected_version: &str,
) -> Option<Vec<VersionTargetMismatch>> {
    let targets = component.version_targets.as_ref()?;
    if targets.is_empty() {
        return None;
    }

    let mut mismatches = Vec::new();
    for target in targets {
        let found = version::read_local_version(&component.local_path, target);
        if found.as_deref() != Some(expected_version) {
            mismatches.push(VersionTargetMismatch {
                file: target.file.clone(),
                expected: expected_version.to_string(),
                found,
            });
        }
    }

    if mismatches.is_empty() {
        None
    } else {
        Some(mismatches)
    }
}

/// Re-read every version target from HEAD's tree (not the working tree) and
/// return any that don't show `expected_version`. Returns `None` when every
/// target matches.
///
/// Used as the final gate before `git.tag` — confirms the version bump was
/// actually committed, not just written to the working tree. This catches
/// the orphan-tag pattern even if `git.commit` is somehow skipped or amended
/// to the wrong commit.
pub(crate) fn collect_head_version_mismatches(
    component: &Component,
    expected_version: &str,
) -> Option<Vec<VersionTargetMismatch>> {
    let targets = component.version_targets.as_ref()?;
    if targets.is_empty() {
        return None;
    }

    let mut mismatches = Vec::new();
    for target in targets {
        let found = read_version_at_head(component, target);
        if found.as_deref() != Some(expected_version) {
            mismatches.push(VersionTargetMismatch {
                file: target.file.clone(),
                expected: expected_version.to_string(),
                found,
            });
        }
    }

    if mismatches.is_empty() {
        None
    } else {
        Some(mismatches)
    }
}

/// Resolve the git toplevel directory for `path`. Returns `None` if `path`
/// is not inside a git repo or if the git invocation fails for any reason.
/// Used to translate `component.local_path` into a stripping root for
/// `git show HEAD:<rel>`, which always resolves `<rel>` against the
/// repository toplevel regardless of cwd.
fn git_toplevel(path: &str) -> Option<std::path::PathBuf> {
    let output =
        crate::core::git::execute_git_for_release(path, &["rev-parse", "--show-toplevel"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(trimmed))
}

/// Read a version target's content from `HEAD` (committed tree) and parse
/// the version string out of it. Returns `None` if the file is missing at
/// HEAD, the git command fails, or the content can't be parsed.
fn read_version_at_head(component: &Component, target: &VersionTarget) -> Option<String> {
    use crate::core::release::version::{
        default_pattern_for_file, parse_version, resolve_version_file_path,
    };

    let pattern = target
        .pattern
        .clone()
        .or_else(|| default_pattern_for_file(&target.file))?;

    // Resolve the path the same way bump_component_version does, then make it
    // relative to the git toplevel for `git show HEAD:<rel>`. `git show`
    // resolves `<rel>` against the repository toplevel — NOT against the
    // current working directory — so for monorepo-scoped components whose
    // `local_path` is a subdirectory of the toplevel we MUST strip the
    // toplevel, not `local_path`. Stripping `local_path` produced a
    // toplevel-incomplete path, which `git show` rejected. See #2327.
    //
    // For root-layout components `local_path` *is* the toplevel, so the
    // toplevel-relative path equals the `local_path`-relative path and
    // behavior is unchanged.
    //
    // We canonicalize both sides before stripping so that platform symlinks
    // (notably macOS `/var` → `/private/var`) don't defeat the prefix match
    // when `full_path` and the git toplevel were derived through different
    // resolution paths.
    //
    // `git show` also requires forward slashes and rejects absolute paths.
    let full_path = resolve_version_file_path(&component.local_path, &target.file);
    let strip_root = git_toplevel(&component.local_path)
        .unwrap_or_else(|| std::path::PathBuf::from(&component.local_path));
    let canonical_full =
        std::fs::canonicalize(&full_path).unwrap_or_else(|_| std::path::PathBuf::from(&full_path));
    let canonical_root = std::fs::canonicalize(&strip_root).unwrap_or_else(|_| strip_root.clone());
    let rel_path = canonical_full
        .strip_prefix(&canonical_root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");

    let spec = format!("HEAD:{}", rel_path);
    let output =
        crate::core::git::execute_git_for_release(&component.local_path, &["show", &spec]).ok()?;
    if !output.status.success() {
        return None;
    }

    let content = String::from_utf8(output.stdout).ok()?;
    let normalized_pattern = crate::core::component::normalize_version_pattern(&pattern);
    parse_version(&content, &normalized_pattern)
}
