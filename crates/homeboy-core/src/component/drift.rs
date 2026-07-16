//! Merge-aftermath drift file resolution.
//!
//! "Drift files" are repo-relative paths whose changes on the base branch
//! after a merge are bookkeeping, not authored changes:
//!
//! - The audit baseline (`homeboy.json`) — refreshed by `homeboy audit
//!   --baseline` when the audit signal shifts.
//! - Extension-declared generated dependency metadata, refreshed by the
//!   extension's own build or install process after a release version bump.
//!
//! The CI autofix pipeline ([Extra-Chill/homeboy-action]) treats drift as
//! review-worthless: drift-only changes are pushed directly to the base
//! branch instead of opened as PRs. The action queries this resolver via
//! `homeboy component show`, which exposes a computed `drift_files` array.
//!
//! ## Sources
//!
//! 1. **Always:** the component's `homeboy.json` (the audit baseline file).
//! 2. **Extension-declared:** `build.lockfile_paths` from each extension
//!    enabled on the component.
//! 3. **Component-declared:** `extra_drift_files` on the component config.
//!
//! Component entries are additive only — they cannot remove extension-
//! declared drift. This keeps extension policy authoritative.
//!
//! ## Path semantics
//!
//! - All paths are repo-root-relative.
//! - Absolute paths are rejected (returned as `None` from normalization).
//! - The resolver does not check existence; the caller decides whether to
//!   skip missing paths. This keeps the resolver pure and testable.
//!
//! [Extra-Chill/homeboy-action]: https://github.com/Extra-Chill/homeboy-action

use crate::component::Component;
use std::collections::BTreeSet;
use std::path::Path;

/// Resolve the merge-aftermath drift file list for a component.
///
/// Returns repo-root-relative paths in deterministic (sorted) order. The
/// audit baseline (`homeboy.json`) is always first; remaining entries are
/// the union of extension-declared lockfiles and component-declared extras.
///
/// Extension manifests are loaded via [`crate::extension::load_extension`].
/// Extensions that fail to load are silently skipped — drift resolution is
/// a non-essential side query and should not break callers.
pub fn drift_file_paths(component: &Component) -> Vec<String> {
    let mut extras: BTreeSet<String> = BTreeSet::new();

    if let Some(extensions) = component.extensions.as_ref() {
        for extension_id in extensions.keys() {
            let Ok(manifest) = crate::extension::load_extension(extension_id) else {
                continue;
            };
            let Some(build) = manifest.build.as_ref() else {
                continue;
            };
            for path in &build.lockfile_paths {
                if let Some(normalized) = normalize_relative_drift_path(path) {
                    extras.insert(normalized);
                }
            }
        }
    }

    for path in &component.extra_drift_files {
        if let Some(normalized) = normalize_relative_drift_path(path) {
            extras.insert(normalized);
        }
    }

    let mut out = Vec::with_capacity(extras.len() + 1);
    out.push("homeboy.json".to_string());
    out.extend(extras);
    out
}

/// Normalize a drift path into a repo-root-relative POSIX-style string.
///
/// Returns `None` for absolute paths or paths containing `..`/prefix
/// components. Empty paths are also rejected.
fn normalize_relative_drift_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return None;
    }

    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }

    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Top-level coverage entry for `drift_file_paths` — the audit
    /// convention `test_coverage::missing_test_method` expects a
    /// `test_<fn_name>` test for every public function. The detailed
    /// behavior is exercised by the named tests below; this one asserts
    /// the public contract.
    #[test]
    fn test_drift_file_paths() {
        let mut component = Component::default();
        component.extra_drift_files = vec!["build/version.txt".to_string()];

        let paths = drift_file_paths(&component);

        // Audit baseline always comes first.
        assert_eq!(paths.first().map(String::as_str), Some("homeboy.json"));
        // Component extras land in the deduped tail.
        assert!(paths.iter().any(|p| p == "build/version.txt"));
        // No duplicates.
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), paths.len());
    }

    #[test]
    fn normalize_strips_curdir_and_rejects_absolute() {
        assert_eq!(
            normalize_relative_drift_path("Cargo.lock").as_deref(),
            Some("Cargo.lock")
        );
        assert_eq!(
            normalize_relative_drift_path("./Cargo.lock").as_deref(),
            Some("Cargo.lock")
        );
        assert_eq!(
            normalize_relative_drift_path("packages/sdk/composer.lock").as_deref(),
            Some("packages/sdk/composer.lock")
        );
        assert!(normalize_relative_drift_path("/etc/passwd").is_none());
        assert!(normalize_relative_drift_path("../escape").is_none());
        assert!(normalize_relative_drift_path("").is_none());
    }

    #[test]
    fn drift_list_has_homeboy_json_first_when_no_extensions() {
        let component = Component::default();
        let paths = drift_file_paths(&component);
        assert_eq!(paths, vec!["homeboy.json".to_string()]);
    }

    #[test]
    fn drift_list_includes_component_extras_sorted() {
        let mut component = Component::default();
        component.extra_drift_files = vec!["zzz.gen.json".to_string(), "aaa.gen.txt".to_string()];

        let paths = drift_file_paths(&component);
        assert_eq!(
            paths,
            vec![
                "homeboy.json".to_string(),
                "aaa.gen.txt".to_string(),
                "zzz.gen.json".to_string(),
            ]
        );
    }

    #[test]
    fn drift_list_dedupes_overlapping_entries() {
        let mut component = Component::default();
        component.extra_drift_files = vec![
            "Cargo.lock".to_string(),
            "./Cargo.lock".to_string(),
            "Cargo.lock".to_string(),
        ];

        let paths = drift_file_paths(&component);
        assert_eq!(
            paths,
            vec!["homeboy.json".to_string(), "Cargo.lock".to_string()]
        );
    }

    #[test]
    fn drift_list_silently_drops_invalid_paths() {
        let mut component = Component::default();
        component.extra_drift_files = vec![
            "Cargo.lock".to_string(),
            "/absolute".to_string(),
            "../escape".to_string(),
            "".to_string(),
        ];

        let paths = drift_file_paths(&component);
        assert_eq!(
            paths,
            vec!["homeboy.json".to_string(), "Cargo.lock".to_string()]
        );
    }
}
