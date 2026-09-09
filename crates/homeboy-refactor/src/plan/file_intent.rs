//! Decompose conflict resolution for the fix pipeline.
//!
//! When multiple fixers target the same file, their modifications can conflict.
//! For example, `ImportAdd` adds explicit imports that decompose's `pub use *`
//! re-exports already cover, and `VisibilityChange` narrows visibility that
//! decompose needs to keep wide for re-export paths.
//!
//! `DecomposeConflictMap` records files that will be decomposed and drops
//! content fixes that conflict with decompose's re-export mechanism.

use std::collections::HashSet;

use crate::auto::contracts::{Fix, InsertionKind};

/// Files whose content fixes may conflict with a planned decomposition.
#[derive(Debug, Default)]
pub(crate) struct DecomposeConflictMap {
    files: HashSet<String>,
}

impl DecomposeConflictMap {
    pub(crate) fn new() -> Self {
        Self {
            files: HashSet::new(),
        }
    }

    pub(crate) fn mark_decompose(&mut self, file: String) {
        self.files.insert(file);
    }

    /// Remove insertions that conflict with decompose's generated re-exports.
    pub(crate) fn resolve_conflicts(&self, fixes: &mut Vec<Fix>) -> usize {
        let mut total_dropped = 0;

        for fix in fixes.iter_mut() {
            if !self.files.contains(&fix.file) {
                continue;
            }

            let before = fix.insertions.len();
            fix.insertions.retain(|insertion| {
                let dominated = is_dominated_by_decompose(&insertion.kind);
                if dominated {
                    eprintln!(
                        "Conflict resolution: dropped {} on {} (dominated by decompose)",
                        insertion.description, fix.file
                    );
                }
                !dominated
            });
            total_dropped += before - fix.insertions.len();
        }

        // Remove fixes that have no insertions left after conflict resolution.
        fixes.retain(|fix| !fix.insertions.is_empty());

        total_dropped
    }
}

fn is_dominated_by_decompose(kind: &InsertionKind) -> bool {
    matches!(
        kind,
        InsertionKind::ImportAdd
            | InsertionKind::VisibilityChange { .. }
            | InsertionKind::ReexportRemoval { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto::contracts::{Fix, Insertion, InsertionKind};
    use homeboy_audit_contract::AuditFinding;

    fn make_fix(file: &str, kind: InsertionKind) -> Fix {
        Fix {
            file: file.to_string(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![Insertion {
                primitive: None,
                kind,
                finding: AuditFinding::MissingImport,
                manual_only: false,
                auto_apply: true,
                blocked_reason: None,
                code: String::new(),
                description: "test fix".to_string(),
            }],
            applied: false,
        }
    }

    #[test]
    fn normal_intent_keeps_all_fixes() {
        let map = DecomposeConflictMap::new();
        let mut fixes = vec![
            make_fix("src/foo.rs", InsertionKind::ImportAdd),
            make_fix(
                "src/foo.rs",
                InsertionKind::VisibilityChange {
                    line: 1,
                    from: "pub fn".into(),
                    to: "pub(crate) fn".into(),
                },
            ),
        ];
        let dropped = map.resolve_conflicts(&mut fixes);
        assert_eq!(dropped, 0);
        assert_eq!(fixes.len(), 2);
    }

    #[test]
    fn decompose_drops_import_add_and_visibility() {
        let mut map = DecomposeConflictMap::new();
        map.mark_decompose("src/foo.rs".into());

        let mut fixes = vec![
            make_fix("src/foo.rs", InsertionKind::ImportAdd),
            make_fix(
                "src/foo.rs",
                InsertionKind::VisibilityChange {
                    line: 1,
                    from: "pub fn".into(),
                    to: "pub(crate) fn".into(),
                },
            ),
            make_fix("src/bar.rs", InsertionKind::ImportAdd), // different file — kept
        ];
        let dropped = map.resolve_conflicts(&mut fixes);
        assert_eq!(dropped, 2);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].file, "src/bar.rs");
    }

    #[test]
    fn decompose_keeps_non_dominated_kinds() {
        let mut map = DecomposeConflictMap::new();
        map.mark_decompose("src/foo.rs".into());

        let mut fixes = vec![make_fix("src/foo.rs", InsertionKind::MethodStub)];
        let dropped = map.resolve_conflicts(&mut fixes);
        assert_eq!(dropped, 0);
        assert_eq!(fixes.len(), 1);
    }
}
