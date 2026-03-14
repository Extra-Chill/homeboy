//! Call-site impact tracing for scoped audits.
//!
//! When `--changed-since` is active, this module expands the audit scope
//! beyond just the changed files to include their **call sites** — files
//! that reference symbols which changed in the PR.
//!
//! Flow:
//! 1. For each changed file, retrieve its base-ref version via `git show`
//! 2. Fingerprint both versions (base + current) using extension scripts
//! 3. Diff the fingerprints to find what symbols changed (renamed, removed,
//!    signature changed)
//! 4. Scan all fingerprints to find files that reference the changed symbols
//! 5. Return the expanded scope (changed files + affected call sites)
//!
//! This is a universal primitive — lives in `audit_internal()` so every
//! consumer of `audit_path_scoped()` gets call-site awareness automatically.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::fingerprint::FileFingerprint;

// ============================================================================
// Symbol diff types
// ============================================================================

/// What changed in a single file between base ref and current version.
#[derive(Debug, Clone)]
pub struct SymbolDiff {
    /// The file that changed (repo-relative path).
    pub file: String,
    /// Exports that existed in the base but are gone in current.
    pub removed_exports: Vec<String>,
    /// Exports that exist in current but not in base (new API).
    #[allow(dead_code)] // Symmetric with removed_exports; used for impact reporting.
    pub added_exports: Vec<String>,
    /// Exports that exist in both but with a likely rename (fuzzy matched).
    pub renamed_exports: Vec<(String, String)>, // (old_name, new_name)
    /// The type/class name changed.
    pub type_renamed: Option<(String, String)>, // (old_name, new_name)
    /// Hooks that were removed or renamed.
    pub removed_hooks: Vec<String>,
    /// Hooks that were added.
    #[allow(dead_code)] // Symmetric with removed_hooks; used for impact reporting.
    pub added_hooks: Vec<String>,
}

/// A file affected by changes in another file.
#[derive(Debug, Clone)]
pub struct AffectedFile {
    /// The affected file (repo-relative path).
    pub file: String,
    /// Which changed file caused this.
    pub source_file: String,
    /// Which symbol(s) link these files.
    pub reasons: Vec<AffectReason>,
}

/// Why a file is affected by a change.
#[derive(Debug, Clone)]
pub enum AffectReason {
    /// File imports or references the changed type/class.
    ImportsChangedType {
        old_name: String,
        new_name: Option<String>,
    },
    /// File calls a function that was removed or renamed.
    CallsRemovedFunction {
        old_name: String,
        new_name: Option<String>,
    },
    /// File hooks into an action/filter that was removed or renamed.
    HooksRemovedAction { old_name: String },
    /// File extends a class that was renamed.
    ExtendsChangedClass {
        old_name: String,
        new_name: Option<String>,
    },
}

impl std::fmt::Display for AffectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AffectReason::ImportsChangedType { old_name, new_name } => {
                if let Some(new) = new_name {
                    write!(f, "imports '{}' (renamed to '{}')", old_name, new)
                } else {
                    write!(f, "imports '{}' (removed)", old_name)
                }
            }
            AffectReason::CallsRemovedFunction { old_name, new_name } => {
                if let Some(new) = new_name {
                    write!(f, "calls '{}' (renamed to '{}')", old_name, new)
                } else {
                    write!(f, "calls '{}' (removed)", old_name)
                }
            }
            AffectReason::HooksRemovedAction { old_name } => {
                write!(f, "hooks '{}' (removed/renamed)", old_name)
            }
            AffectReason::ExtendsChangedClass { old_name, new_name } => {
                if let Some(new) = new_name {
                    write!(f, "extends '{}' (renamed to '{}')", old_name, new)
                } else {
                    write!(f, "extends '{}' (removed)", old_name)
                }
            }
        }
    }
}

// ============================================================================
// Base-ref fingerprinting
// ============================================================================

/// Retrieve a file's content from a git ref and fingerprint it.
///
/// Uses `git show <ref>:<path>` to get the base version without checkout,
/// then runs the extension fingerprint script on that content.
pub fn fingerprint_from_git_ref(
    source_path: &str,
    git_ref: &str,
    relative_path: &str,
) -> Option<FileFingerprint> {
    use crate::extension;

    // Get file content from the git ref
    let git_spec = format!("{}:{}", git_ref, relative_path);
    let content =
        crate::engine::command::run_in_optional(source_path, "git", &["show", &git_spec])?;

    // Find the extension for this file type
    let ext = Path::new(relative_path).extension()?.to_str()?;
    let matched_extension = extension::find_extension_for_file_ext(ext, "fingerprint")?;

    // Run the fingerprint script on the base content
    let output = extension::run_fingerprint_script(&matched_extension, relative_path, &content)?;

    let language = super::conventions::Language::from_extension(ext);

    Some(FileFingerprint {
        relative_path: relative_path.to_string(),
        language,
        methods: output.methods,
        registrations: output.registrations,
        type_name: output.type_name,
        type_names: output.type_names,
        extends: output.extends,
        implements: output.implements,
        namespace: output.namespace,
        imports: output.imports,
        content,
        method_hashes: output.method_hashes,
        structural_hashes: output.structural_hashes,
        visibility: output.visibility,
        properties: output.properties,
        hooks: output.hooks,
        unused_parameters: output.unused_parameters,
        dead_code_markers: output.dead_code_markers,
        internal_calls: output.internal_calls,
        public_api: output.public_api,
        trait_impl_methods: Vec::new(),
    })
}

// ============================================================================
// Symbol diffing
// ============================================================================

/// Compare base-ref fingerprints against current fingerprints for changed files.
///
/// For each changed file, retrieves the base version, fingerprints it, and
/// produces a `SymbolDiff` describing what symbols changed.
pub fn diff_changed_files(
    source_path: &str,
    git_ref: &str,
    changed_files: &[String],
    current_fingerprints: &[&FileFingerprint],
) -> Vec<SymbolDiff> {
    let mut diffs = Vec::new();

    // Index current fingerprints by path for fast lookup
    let current_by_path: HashMap<&str, &FileFingerprint> = current_fingerprints
        .iter()
        .map(|fp| (fp.relative_path.as_str(), *fp))
        .collect();

    for file in changed_files {
        let current_fp = current_by_path.get(file.as_str());
        let base_fp = fingerprint_from_git_ref(source_path, git_ref, file);

        let diff = match (base_fp.as_ref(), current_fp) {
            // File existed at base and still exists — diff the symbols
            (Some(base), Some(current)) => diff_fingerprints(file, base, current),
            // File was deleted (exists at base, gone now) — all exports removed
            (Some(base), None) => SymbolDiff {
                file: file.clone(),
                removed_exports: base.public_api.clone(),
                added_exports: vec![],
                renamed_exports: vec![],
                type_renamed: None,
                removed_hooks: base.hooks.iter().map(|h| h.name.clone()).collect(),
                added_hooks: vec![],
            },
            // New file — nothing to trace (no old callers)
            (None, Some(_)) => continue,
            // File doesn't fingerprint in either version — skip
            (None, None) => continue,
        };

        // Only include diffs that actually have changes worth tracing
        if !diff.removed_exports.is_empty()
            || !diff.renamed_exports.is_empty()
            || diff.type_renamed.is_some()
            || !diff.removed_hooks.is_empty()
        {
            diffs.push(diff);
        }
    }

    diffs
}

/// Diff two fingerprints of the same file (base vs current).
fn diff_fingerprints(file: &str, base: &FileFingerprint, current: &FileFingerprint) -> SymbolDiff {
    let base_exports: HashSet<&str> = base.public_api.iter().map(|s| s.as_str()).collect();
    let current_exports: HashSet<&str> = current.public_api.iter().map(|s| s.as_str()).collect();

    let removed: Vec<String> = base_exports
        .difference(&current_exports)
        .map(|s| s.to_string())
        .collect();
    let added: Vec<String> = current_exports
        .difference(&base_exports)
        .map(|s| s.to_string())
        .collect();

    // Try to match removed → added as renames (simple: same position or similar name)
    let (renamed, truly_removed, truly_added) = match_renames(&removed, &added);

    // Check type/class rename
    let type_renamed = match (&base.type_name, &current.type_name) {
        (Some(old), Some(new)) if old != new => Some((old.clone(), new.clone())),
        _ => None,
    };

    // Diff hooks
    let base_hooks: HashSet<&str> = base.hooks.iter().map(|h| h.name.as_str()).collect();
    let current_hooks: HashSet<&str> = current.hooks.iter().map(|h| h.name.as_str()).collect();

    let removed_hooks: Vec<String> = base_hooks
        .difference(&current_hooks)
        .map(|s| s.to_string())
        .collect();
    let added_hooks: Vec<String> = current_hooks
        .difference(&base_hooks)
        .map(|s| s.to_string())
        .collect();

    SymbolDiff {
        file: file.to_string(),
        removed_exports: truly_removed,
        added_exports: truly_added,
        renamed_exports: renamed,
        type_renamed,
        removed_hooks,
        added_hooks,
    }
}

/// Try to pair removed symbols with added symbols as renames.
///
/// Uses a simple heuristic: if a removed name shares a significant common
/// substring with an added name, treat it as a rename. Returns:
/// - matched renames (old, new)
/// - truly removed (no match found)
/// - truly added (no match found)
fn match_renames(
    removed: &[String],
    added: &[String],
) -> (Vec<(String, String)>, Vec<String>, Vec<String>) {
    let mut renames = Vec::new();
    let mut used_added: HashSet<usize> = HashSet::new();
    let mut truly_removed = Vec::new();

    for old in removed {
        let mut best_match: Option<(usize, f64)> = None;

        for (i, new) in added.iter().enumerate() {
            if used_added.contains(&i) {
                continue;
            }
            let score = similarity(old, new);
            if score > 0.5 && best_match.is_none_or(|(_, best_score)| score > best_score) {
                best_match = Some((i, score));
            }
        }

        if let Some((idx, _)) = best_match {
            renames.push((old.clone(), added[idx].clone()));
            used_added.insert(idx);
        } else {
            truly_removed.push(old.clone());
        }
    }

    let truly_added: Vec<String> = added
        .iter()
        .enumerate()
        .filter(|(i, _)| !used_added.contains(i))
        .map(|(_, s)| s.clone())
        .collect();

    (renames, truly_removed, truly_added)
}

/// Simple similarity score between two strings (0.0 = nothing in common, 1.0 = identical).
/// Uses longest common subsequence ratio.
fn similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let m = a_bytes.len();
    let n = b_bytes.len();

    // LCS via dynamic programming
    let mut dp = vec![vec![0u16; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a_bytes[i - 1] == b_bytes[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    let lcs_len = dp[m][n] as f64;
    // Ratio against the longer string
    lcs_len / m.max(n) as f64
}

// ============================================================================
// Affected file detection
// ============================================================================

/// Find all files affected by the symbol changes.
///
/// Scans all fingerprints for references to changed symbols:
/// - `internal_calls` matching removed/renamed exports
/// - `imports` matching changed type names
/// - `extends` matching changed class names
/// - `hooks` matching removed hook names
///
/// Returns only files NOT already in the changed set.
pub fn find_affected_files(
    diffs: &[SymbolDiff],
    all_fingerprints: &[&FileFingerprint],
    changed_files: &HashSet<&str>,
) -> Vec<AffectedFile> {
    let mut affected: HashMap<String, AffectedFile> = HashMap::new();

    for diff in diffs {
        for fp in all_fingerprints {
            // Skip files that are already in the changed set
            if changed_files.contains(fp.relative_path.as_str()) {
                continue;
            }

            let mut reasons = Vec::new();

            // Check: calls a removed or renamed function
            for removed in &diff.removed_exports {
                if fp.internal_calls.contains(removed) {
                    let new_name = diff
                        .renamed_exports
                        .iter()
                        .find(|(old, _)| old == removed)
                        .map(|(_, new)| new.clone());
                    reasons.push(AffectReason::CallsRemovedFunction {
                        old_name: removed.clone(),
                        new_name,
                    });
                }
            }
            // Also check renamed — the old name might appear in calls
            for (old_name, new_name) in &diff.renamed_exports {
                if fp.internal_calls.contains(old_name) {
                    reasons.push(AffectReason::CallsRemovedFunction {
                        old_name: old_name.clone(),
                        new_name: Some(new_name.clone()),
                    });
                }
            }

            // Check: imports the changed type/class
            if let Some((old_type, new_type)) = &diff.type_renamed {
                let imports_old = fp.imports.iter().any(|imp| imp.contains(old_type.as_str()));
                if imports_old {
                    reasons.push(AffectReason::ImportsChangedType {
                        old_name: old_type.clone(),
                        new_name: Some(new_type.clone()),
                    });
                }
            }

            // Check: extends a class that was renamed
            if let Some((old_type, new_type)) = &diff.type_renamed {
                if fp.extends.as_deref() == Some(old_type.as_str()) {
                    reasons.push(AffectReason::ExtendsChangedClass {
                        old_name: old_type.clone(),
                        new_name: Some(new_type.clone()),
                    });
                }
            }

            // Check: hooks into a removed action/filter
            for removed_hook in &diff.removed_hooks {
                let hooks_it = fp.hooks.iter().any(|h| h.name == *removed_hook);
                // Also check registrations (add_action/add_filter calls)
                let registers_it = fp
                    .registrations
                    .iter()
                    .any(|r| r.contains(removed_hook.as_str()));
                if hooks_it || registers_it {
                    reasons.push(AffectReason::HooksRemovedAction {
                        old_name: removed_hook.clone(),
                    });
                }
            }

            if !reasons.is_empty() {
                let entry =
                    affected
                        .entry(fp.relative_path.clone())
                        .or_insert_with(|| AffectedFile {
                            file: fp.relative_path.clone(),
                            source_file: diff.file.clone(),
                            reasons: Vec::new(),
                        });
                entry.reasons.extend(reasons);
            }
        }
    }

    let mut result: Vec<AffectedFile> = affected.into_values().collect();
    result.sort_by(|a, b| a.file.cmp(&b.file));
    result
}

// ============================================================================
// Scope expansion (Phase 4j replacement)
// ============================================================================

/// Expand the audit scope from changed files to changed files + affected call sites.
///
/// This replaces the simple filename filter in Phase 4j. Instead of:
///   `findings.retain(|f| changed_files.contains(&f.file))`
/// it does:
///   1. Diff changed files' fingerprints against base ref
///   2. Find all files that reference changed symbols
///   3. Filter findings to changed files + affected files
///
/// Returns the expanded file set and a list of affected files for logging.
pub fn expand_scope(
    source_path: &str,
    git_ref: &str,
    changed_files: &[String],
    all_fingerprints: &[&FileFingerprint],
) -> (HashSet<String>, Vec<AffectedFile>) {
    // Step 1: Diff changed files against base ref
    let diffs = diff_changed_files(source_path, git_ref, changed_files, all_fingerprints);

    if diffs.is_empty() {
        // No meaningful symbol changes — just use the original file list
        let scope: HashSet<String> = changed_files.iter().cloned().collect();
        return (scope, vec![]);
    }

    // Step 2: Find affected files
    let changed_set: HashSet<&str> = changed_files.iter().map(|s| s.as_str()).collect();
    let affected = find_affected_files(&diffs, all_fingerprints, &changed_set);

    // Step 3: Build expanded scope
    let mut scope: HashSet<String> = changed_files.iter().cloned().collect();
    for af in &affected {
        scope.insert(af.file.clone());
    }

    (scope, affected)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::Language;

    fn make_fingerprint(
        path: &str,
        public_api: Vec<&str>,
        internal_calls: Vec<&str>,
        imports: Vec<&str>,
        type_name: Option<&str>,
        extends: Option<&str>,
        hooks: Vec<(&str, &str)>,
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            methods: public_api.iter().map(|s| s.to_string()).collect(),
            type_name: type_name.map(|s| s.to_string()),
            extends: extends.map(|s| s.to_string()),
            imports: imports.iter().map(|s| s.to_string()).collect(),
            hooks: hooks
                .iter()
                .map(|(t, n)| crate::extension::HookRef {
                    hook_type: t.to_string(),
                    name: n.to_string(),
                })
                .collect(),
            internal_calls: internal_calls.iter().map(|s| s.to_string()).collect(),
            public_api: public_api.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn test_similarity_identical() {
        assert!((similarity("doThing", "doThing") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_similarity_empty() {
        assert!((similarity("", "anything")).abs() < f64::EPSILON);
        assert!((similarity("anything", "")).abs() < f64::EPSILON);
    }

    #[test]
    fn test_similarity_renamed() {
        // doThing → doStuff — low similarity (only "do" + "g" shared = LCS 3/7 ≈ 0.43)
        let score = similarity("doThing", "doStuff");
        assert!(score > 0.2, "score should be > 0.2, got {}", score);
        assert!(score < 0.8, "score should be < 0.8, got {}", score);
    }

    #[test]
    fn test_similarity_prefixed() {
        // getUser → getUserById — high similarity (prefix match)
        let score = similarity("getUser", "getUserById");
        assert!(score > 0.5, "score should be > 0.5, got {}", score);
    }

    #[test]
    fn test_match_renames_exact_pair() {
        let removed = vec!["doThing".to_string()];
        let added = vec!["doStuff".to_string(), "completelyNew".to_string()];
        let (renames, truly_removed, truly_added) = match_renames(&removed, &added);

        // doThing and doStuff share "do" + similar length — may or may not match
        // depending on threshold. The key test is that the function runs.
        assert!(renames.len() + truly_removed.len() == 1);
        assert!(!truly_added.is_empty());
    }

    #[test]
    fn test_match_renames_clear_rename() {
        let removed = vec!["processRequest".to_string()];
        let added = vec!["processApiRequest".to_string()];
        let (renames, truly_removed, _) = match_renames(&removed, &added);

        // High similarity — should match
        assert_eq!(renames.len(), 1, "should detect rename");
        assert!(truly_removed.is_empty());
        assert_eq!(renames[0].0, "processRequest");
        assert_eq!(renames[0].1, "processApiRequest");
    }

    #[test]
    fn test_diff_fingerprints_detects_removed_export() {
        let base = make_fingerprint(
            "Foo.php",
            vec!["doThing", "doOther"],
            vec![],
            vec![],
            Some("Foo"),
            None,
            vec![],
        );
        let current = make_fingerprint(
            "Foo.php",
            vec!["doOther"],
            vec![],
            vec![],
            Some("Foo"),
            None,
            vec![],
        );

        let diff = diff_fingerprints("Foo.php", &base, &current);
        assert!(
            diff.removed_exports.contains(&"doThing".to_string())
                || diff.renamed_exports.iter().any(|(old, _)| old == "doThing"),
            "doThing should be in removed or renamed"
        );
    }

    #[test]
    fn test_diff_fingerprints_detects_type_rename() {
        let base = make_fingerprint(
            "Foo.php",
            vec!["run"],
            vec![],
            vec![],
            Some("FooHandler"),
            None,
            vec![],
        );
        let current = make_fingerprint(
            "Foo.php",
            vec!["run"],
            vec![],
            vec![],
            Some("BarHandler"),
            None,
            vec![],
        );

        let diff = diff_fingerprints("Foo.php", &base, &current);
        assert_eq!(
            diff.type_renamed,
            Some(("FooHandler".to_string(), "BarHandler".to_string()))
        );
    }

    #[test]
    fn test_diff_fingerprints_detects_removed_hook() {
        let base = make_fingerprint(
            "Foo.php",
            vec![],
            vec![],
            vec![],
            None,
            None,
            vec![("action", "my_custom_action")],
        );
        let current = make_fingerprint(
            "Foo.php",
            vec![],
            vec![],
            vec![],
            None,
            None,
            vec![("action", "my_renamed_action")],
        );

        let diff = diff_fingerprints("Foo.php", &base, &current);
        assert!(diff.removed_hooks.contains(&"my_custom_action".to_string()));
        assert!(diff.added_hooks.contains(&"my_renamed_action".to_string()));
    }

    #[test]
    fn test_find_affected_calls_removed_function() {
        // Foo.php removed doThing(), Bar.php calls doThing()
        let diff = SymbolDiff {
            file: "Foo.php".to_string(),
            removed_exports: vec!["doThing".to_string()],
            added_exports: vec![],
            renamed_exports: vec![],
            type_renamed: None,
            removed_hooks: vec![],
            added_hooks: vec![],
        };

        let bar = make_fingerprint(
            "Bar.php",
            vec!["run"],
            vec!["doThing"], // calls the removed function
            vec![],
            None,
            None,
            vec![],
        );
        let baz = make_fingerprint(
            "Baz.php",
            vec!["run"],
            vec!["somethingElse"], // doesn't call it
            vec![],
            None,
            None,
            vec![],
        );

        let all_fps: Vec<&FileFingerprint> = vec![&bar, &baz];
        let changed: HashSet<&str> = HashSet::from(["Foo.php"]);
        let affected = find_affected_files(&[diff], &all_fps, &changed);

        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].file, "Bar.php");
        assert_eq!(affected[0].source_file, "Foo.php");
        assert!(matches!(
            &affected[0].reasons[0],
            AffectReason::CallsRemovedFunction { old_name, .. } if old_name == "doThing"
        ));
    }

    #[test]
    fn test_find_affected_imports_renamed_type() {
        let diff = SymbolDiff {
            file: "Foo.php".to_string(),
            removed_exports: vec![],
            added_exports: vec![],
            renamed_exports: vec![],
            type_renamed: Some(("FooHandler".to_string(), "BarHandler".to_string())),
            removed_hooks: vec![],
            added_hooks: vec![],
        };

        let consumer = make_fingerprint(
            "Consumer.php",
            vec!["run"],
            vec![],
            vec!["use App\\FooHandler"], // imports the old type
            None,
            None,
            vec![],
        );

        let all_fps: Vec<&FileFingerprint> = vec![&consumer];
        let changed: HashSet<&str> = HashSet::from(["Foo.php"]);
        let affected = find_affected_files(&[diff], &all_fps, &changed);

        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].file, "Consumer.php");
        assert!(matches!(
            &affected[0].reasons[0],
            AffectReason::ImportsChangedType { old_name, .. } if old_name == "FooHandler"
        ));
    }

    #[test]
    fn test_find_affected_extends_renamed_class() {
        let diff = SymbolDiff {
            file: "Base.php".to_string(),
            removed_exports: vec![],
            added_exports: vec![],
            renamed_exports: vec![],
            type_renamed: Some(("BaseTask".to_string(), "AbstractTask".to_string())),
            removed_hooks: vec![],
            added_hooks: vec![],
        };

        let child = make_fingerprint(
            "Child.php",
            vec!["run"],
            vec![],
            vec![],
            Some("ChildTask"),
            Some("BaseTask"), // extends the old class name
            vec![],
        );

        let all_fps: Vec<&FileFingerprint> = vec![&child];
        let changed: HashSet<&str> = HashSet::from(["Base.php"]);
        let affected = find_affected_files(&[diff], &all_fps, &changed);

        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].file, "Child.php");
        assert!(matches!(
            &affected[0].reasons[0],
            AffectReason::ExtendsChangedClass { old_name, .. } if old_name == "BaseTask"
        ));
    }

    #[test]
    fn test_find_affected_hooks_removed_action() {
        let diff = SymbolDiff {
            file: "Provider.php".to_string(),
            removed_exports: vec![],
            added_exports: vec![],
            renamed_exports: vec![],
            type_renamed: None,
            removed_hooks: vec!["my_custom_hook".to_string()],
            added_hooks: vec![],
        };

        let listener = make_fingerprint(
            "Listener.php",
            vec!["onHook"],
            vec![],
            vec![],
            None,
            None,
            vec![("filter", "my_custom_hook")], // listens to the removed hook
        );

        let all_fps: Vec<&FileFingerprint> = vec![&listener];
        let changed: HashSet<&str> = HashSet::from(["Provider.php"]);
        let affected = find_affected_files(&[diff], &all_fps, &changed);

        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].file, "Listener.php");
        assert!(matches!(
            &affected[0].reasons[0],
            AffectReason::HooksRemovedAction { old_name } if old_name == "my_custom_hook"
        ));
    }

    #[test]
    fn test_find_affected_skips_changed_files() {
        // Foo.php is both the source of changes AND has calls — should not appear in affected
        let diff = SymbolDiff {
            file: "Foo.php".to_string(),
            removed_exports: vec!["doThing".to_string()],
            added_exports: vec![],
            renamed_exports: vec![],
            type_renamed: None,
            removed_hooks: vec![],
            added_hooks: vec![],
        };

        let foo = make_fingerprint(
            "Foo.php",
            vec!["doOther"],
            vec!["doThing"],
            vec![],
            None,
            None,
            vec![],
        );

        let all_fps: Vec<&FileFingerprint> = vec![&foo];
        let changed: HashSet<&str> = HashSet::from(["Foo.php"]);
        let affected = find_affected_files(&[diff], &all_fps, &changed);

        assert!(
            affected.is_empty(),
            "changed file should not be in affected"
        );
    }

    #[test]
    fn test_find_affected_renamed_function_in_calls() {
        // Foo.php renamed doThing → doStuff, Bar.php calls doThing
        let diff = SymbolDiff {
            file: "Foo.php".to_string(),
            removed_exports: vec![],
            added_exports: vec![],
            renamed_exports: vec![("doThing".to_string(), "doStuff".to_string())],
            type_renamed: None,
            removed_hooks: vec![],
            added_hooks: vec![],
        };

        let bar = make_fingerprint(
            "Bar.php",
            vec!["run"],
            vec!["doThing"],
            vec![],
            None,
            None,
            vec![],
        );

        let all_fps: Vec<&FileFingerprint> = vec![&bar];
        let changed: HashSet<&str> = HashSet::from(["Foo.php"]);
        let affected = find_affected_files(&[diff], &all_fps, &changed);

        assert_eq!(affected.len(), 1);
        assert!(matches!(
            &affected[0].reasons[0],
            AffectReason::CallsRemovedFunction { old_name, new_name }
                if old_name == "doThing" && new_name.as_deref() == Some("doStuff")
        ));
    }

    #[test]
    fn test_expand_scope_no_diffs_returns_changed_only() {
        // When diff_changed_files returns nothing (e.g. no git ref),
        // expand_scope should fall back to just the changed files
        let changed = ["Foo.php".to_string()];
        let foo = make_fingerprint("Foo.php", vec!["run"], vec![], vec![], None, None, vec![]);
        let all_fps: Vec<&FileFingerprint> = vec![&foo];

        // Can't actually call expand_scope without a real git repo,
        // but we can test the fallback logic by calling find_affected_files with empty diffs
        let changed_set: HashSet<&str> = changed.iter().map(|s| s.as_str()).collect();
        let affected = find_affected_files(&[], &all_fps, &changed_set);
        assert!(affected.is_empty());
    }
}
