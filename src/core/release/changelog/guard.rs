//! Changelog-edit guard.
//!
//! Homeboy owns the changelog completely: entries are generated from
//! conventional-prefixed commits (`feat:` / `fix:` / ...) at release time and
//! the release pipeline rewrites the next-section in place. Hand-editing the
//! tracked changelog file in a feature PR is therefore both pointless (the
//! release run regenerates it) and actively harmful — a single shared
//! append-only file is a guaranteed conflict surface when multiple PRs are in
//! flight against the same branch (#4876).
//!
//! This guard detects when a non-release changeset modifies the component's
//! configured changelog target so callers (`homeboy review`, CI) can steer the
//! contributor back to conventional commits instead of hand-editing the
//! changelog. It is intentionally agnostic: it keys off the component's
//! resolved `changelog_target` and a list of changed paths, with no
//! hardcoded filenames.

use std::path::{Component as PathComponent, Path, PathBuf};

/// Outcome of checking a changeset against the changelog-edit guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogGuardViolation {
    /// The changelog path (as it appeared in the changeset) that was modified.
    pub path: String,
    /// Human-readable steering message explaining the policy and the fix.
    pub message: String,
}

/// Detect whether `changed_files` modifies the configured `changelog_target`.
///
/// `changelog_target` is the component's configured changelog path (relative to
/// the component root, e.g. `docs/changelog.md` or `CHANGELOG.md`). Both the
/// target and the changed paths are normalized so that equivalent spellings
/// (`./docs/changelog.md`, `docs/changelog.md`) compare equal and matching is
/// case-insensitive (changelog filenames are conventionally cased
/// inconsistently across repos).
///
/// Returns `None` when no changed file touches the changelog. When the
/// changelog was modified it returns a [`ChangelogGuardViolation`] carrying a
/// steering message. The caller decides whether to treat it as a warning hint
/// or a hard failure.
pub fn detect_changelog_edit(
    changelog_target: Option<&str>,
    changed_files: &[String],
) -> Option<ChangelogGuardViolation> {
    let target = changelog_target?.trim();
    if target.is_empty() {
        return None;
    }

    let normalized_target = normalize_relative_path(target)?;

    let matched = changed_files
        .iter()
        .find(|candidate| paths_match(&normalized_target, candidate))?;

    Some(ChangelogGuardViolation {
        path: matched.clone(),
        message: steering_message(matched),
    })
}

/// Build the steering message shown when a changeset hand-edits the changelog.
fn steering_message(path: &str) -> String {
    format!(
        "Changeset modifies the changelog ({path}). Homeboy generates changelog \
entries from conventional-prefixed commits (feat:/fix:/...) at release time and \
rewrites this file during `homeboy release` — hand-editing it is overwritten and \
makes the changelog a multi-PR merge-conflict surface. Revert the changelog edit \
and describe the change in the commit message instead."
    )
}

/// True when `normalized_target` (a normalized relative changelog path) refers
/// to the same file as `candidate` (a changed-file path from a git diff).
fn paths_match(normalized_target: &Path, candidate: &str) -> bool {
    match normalize_relative_path(candidate) {
        Some(normalized_candidate) => {
            paths_eq_ignore_case(normalized_target, &normalized_candidate)
        }
        None => false,
    }
}

/// Normalize a relative path by dropping `.` components and collapsing
/// redundant separators, without resolving symlinks or touching the
/// filesystem. Returns `None` for empty or purely `.`-valued inputs.
fn normalize_relative_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            PathComponent::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }

    if normalized.as_os_str().is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Case-insensitive path equality across components. Changelog filenames are
/// cased inconsistently across repos (`CHANGELOG.md`, `changelog.md`), so the
/// guard should not miss an edit purely because of casing differences between
/// the configured target and the changed path.
fn paths_eq_ignore_case(a: &Path, b: &Path) -> bool {
    let a_components = crate::core::paths::path_component_strings(a);
    let b_components = crate::core::paths::path_component_strings(b);
    a_components.len() == b_components.len()
        && a_components
            .iter()
            .zip(b_components.iter())
            .all(|(a_str, b_str)| a_str.eq_ignore_ascii_case(b_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn detects_direct_changelog_edit() {
        let violation = detect_changelog_edit(
            Some("docs/changelog.md"),
            &files(&["src/main.rs", "docs/changelog.md"]),
        )
        .expect("changelog edit should be flagged");
        assert_eq!(violation.path, "docs/changelog.md");
        assert!(violation.message.contains("conventional-prefixed commits"));
    }

    #[test]
    fn ignores_changesets_without_changelog() {
        assert!(detect_changelog_edit(
            Some("docs/changelog.md"),
            &files(&["src/main.rs", "README.md"]),
        )
        .is_none());
    }

    #[test]
    fn matches_despite_dot_slash_and_casing() {
        // Configured target lower-cased, changed path with ./ prefix and
        // different casing — these refer to the same file.
        let violation =
            detect_changelog_edit(Some("./docs/changelog.md"), &files(&["docs/CHANGELOG.md"]))
                .expect("normalized + case-insensitive match should fire");
        assert_eq!(violation.path, "docs/CHANGELOG.md");
    }

    #[test]
    fn matches_root_changelog_target() {
        let violation = detect_changelog_edit(Some("CHANGELOG.md"), &files(&["CHANGELOG.md"]))
            .expect("root changelog edit should be flagged");
        assert_eq!(violation.path, "CHANGELOG.md");
    }

    #[test]
    fn does_not_match_similarly_named_unrelated_file() {
        // A file that merely contains the word "changelog" but is not the
        // configured target must not be flagged.
        assert!(detect_changelog_edit(
            Some("docs/changelog.md"),
            &files(&["docs/changelog-policy.md", "src/changelog/io.rs"]),
        )
        .is_none());
    }

    #[test]
    fn no_target_is_a_noop() {
        assert!(detect_changelog_edit(None, &files(&["docs/changelog.md"])).is_none());
        assert!(detect_changelog_edit(Some("   "), &files(&["docs/changelog.md"])).is_none());
    }

    #[test]
    fn empty_changeset_is_a_noop() {
        assert!(detect_changelog_edit(Some("docs/changelog.md"), &[]).is_none());
    }
}
