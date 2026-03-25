//! File-level intent tracking and conflict resolution for the fix pipeline.
//!
//! When multiple fixers target the same file, their modifications can conflict.
//! For example, `ImportAdd` adds explicit imports that decompose's `pub use *`
//! re-exports already cover, and `VisibilityChange` narrows visibility that
//! decompose needs to keep wide for re-export paths.
//!
//! `FileIntent` captures the highest-priority structural operation planned for
//! each file. `resolve_conflicts()` drops content fixes that are dominated by
//! structural intents, replacing ad-hoc skip sets with declarative rules.

use std::collections::HashMap;

use crate::core::refactor::auto::contracts::{Fix, InsertionKind};

/// What structural operation is planned for a file.
///
/// Intents are ordered by priority: structural operations dominate content
/// modifications because they fundamentally change the file's role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileIntent {
    /// File stays in place. Content fixes apply normally.
    Normal,
    /// File will be decomposed into submodules with `pub use *` re-exports.
    /// Content fixes that modify imports or visibility are dominated because
    /// decompose handles both through its re-export mechanism.
    Decompose,
    /// File will be moved/renamed. Import fixes targeting the old path are stale.
    Move { to: String },
    /// File will be deleted. All content fixes are pointless.
    Delete,
}

impl FileIntent {
    /// Returns which `InsertionKind` categories are dominated (should be dropped)
    /// when this intent is active on a file.
    pub fn dominated_insertion_kinds(&self) -> Vec<DominatedKind> {
        match self {
            FileIntent::Normal => vec![],
            FileIntent::Decompose => vec![
                // Decompose's pub use * re-exports handle imports — explicit
                // ImportAdd would create duplicate name definitions.
                DominatedKind::ByKind(InsertionKindCategory::ImportAdd),
                // Decompose needs items to keep their original visibility so
                // pub use * re-exports work. Narrowing pub → pub(crate) breaks
                // the re-export path for consumers.
                DominatedKind::ByKind(InsertionKindCategory::VisibilityChange),
                // Re-export removal conflicts with decompose generating new
                // pub use * re-exports.
                DominatedKind::ByKind(InsertionKindCategory::ReexportRemoval),
            ],
            FileIntent::Move { .. } => vec![
                // Import fixes targeting the old path will be stale after move.
                DominatedKind::ByKind(InsertionKindCategory::ImportAdd),
            ],
            FileIntent::Delete => vec![
                // Everything is pointless on a file being deleted.
                DominatedKind::All,
            ],
        }
    }
}

/// What category of insertion is dominated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DominatedKind {
    /// A specific insertion kind category.
    ByKind(InsertionKindCategory),
    /// All insertion kinds are dominated.
    All,
}

/// Coarse categories for `InsertionKind`, used for conflict matching.
///
/// We don't match on the full enum (which has struct variants with data)
/// because conflict resolution only needs the category, not the specifics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertionKindCategory {
    ImportAdd,
    VisibilityChange,
    ReexportRemoval,
    FunctionRemoval,
    MethodStub,
    TestModule,
    FileMove,
    Other,
}

impl InsertionKindCategory {
    /// Classify an `InsertionKind` into its coarse category.
    pub fn from_kind(kind: &InsertionKind) -> Self {
        match kind {
            InsertionKind::ImportAdd => Self::ImportAdd,
            InsertionKind::VisibilityChange { .. } => Self::VisibilityChange,
            InsertionKind::ReexportRemoval { .. } => Self::ReexportRemoval,
            InsertionKind::FunctionRemoval { .. } => Self::FunctionRemoval,
            InsertionKind::MethodStub => Self::MethodStub,
            InsertionKind::TestModule => Self::TestModule,
            InsertionKind::FileMove { .. } => Self::FileMove,
            _ => Self::Other,
        }
    }
}

/// Registry of file-level intents.
///
/// Built before fix generation so fixers can query it, and used after
/// generation to resolve conflicts by dropping dominated fixes.
#[derive(Debug, Default)]
pub struct FileIntentMap {
    intents: HashMap<String, FileIntent>,
}

impl FileIntentMap {
    pub fn new() -> Self {
        Self {
            intents: HashMap::new(),
        }
    }

    /// Register a structural intent for a file. Higher-priority intents
    /// overwrite lower-priority ones (Delete > Move > Decompose > Normal).
    pub fn set(&mut self, file: String, intent: FileIntent) {
        let dominated = match self.intents.get(&file) {
            Some(existing) => priority(existing) < priority(&intent),
            None => true,
        };
        if dominated {
            self.intents.insert(file, intent);
        }
    }

    /// Check if a file has a structural intent (anything other than Normal).
    pub fn has_structural_intent(&self, file: &str) -> bool {
        matches!(
            self.intents.get(file),
            Some(FileIntent::Decompose | FileIntent::Move { .. } | FileIntent::Delete)
        )
    }

    /// Get the intent for a file, defaulting to Normal.
    pub fn get(&self, file: &str) -> &FileIntent {
        self.intents.get(file).unwrap_or(&FileIntent::Normal)
    }

    /// Resolve conflicts: remove insertions from fixes that are dominated
    /// by the file's structural intent. Returns the number of insertions dropped.
    pub fn resolve_conflicts(&self, fixes: &mut Vec<Fix>) -> usize {
        let mut total_dropped = 0;

        for fix in fixes.iter_mut() {
            let intent = self.get(&fix.file);
            if *intent == FileIntent::Normal {
                continue;
            }

            let dominated = intent.dominated_insertion_kinds();
            if dominated.is_empty() {
                continue;
            }

            let before = fix.insertions.len();
            fix.insertions.retain(|insertion| {
                let dominated = is_dominated(&insertion.kind, &dominated);
                if dominated {
                    eprintln!(
                        "Conflict resolution: dropped {} on {} (dominated by {:?})",
                        insertion.description, fix.file, intent
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

/// Check if an insertion kind is dominated by any of the dominated-kind rules.
fn is_dominated(kind: &InsertionKind, dominated: &[DominatedKind]) -> bool {
    let category = InsertionKindCategory::from_kind(kind);
    dominated.iter().any(|d| match d {
        DominatedKind::All => true,
        DominatedKind::ByKind(cat) => *cat == category,
    })
}

/// Priority ordering: Delete > Move > Decompose > Normal.
fn priority(intent: &FileIntent) -> u8 {
    match intent {
        FileIntent::Normal => 0,
        FileIntent::Decompose => 1,
        FileIntent::Move { .. } => 2,
        FileIntent::Delete => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::AuditFinding;
    use crate::core::refactor::auto::contracts::{Fix, FixSafetyTier, Insertion, InsertionKind};

    fn make_fix(file: &str, kind: InsertionKind) -> Fix {
        Fix {
            file: file.to_string(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![Insertion {
                kind,
                finding: AuditFinding::MissingImport,
                safety_tier: FixSafetyTier::Safe,
                auto_apply: true,
                blocked_reason: None,
                preflight: None,
                code: String::new(),
                description: "test fix".to_string(),
            }],
            applied: false,
        }
    }

    #[test]
    fn normal_intent_keeps_all_fixes() {
        let map = FileIntentMap::new();
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
        let mut map = FileIntentMap::new();
        map.set("src/foo.rs".into(), FileIntent::Decompose);

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
        let mut map = FileIntentMap::new();
        map.set("src/foo.rs".into(), FileIntent::Decompose);

        let mut fixes = vec![make_fix("src/foo.rs", InsertionKind::MethodStub)];
        let dropped = map.resolve_conflicts(&mut fixes);
        assert_eq!(dropped, 0);
        assert_eq!(fixes.len(), 1);
    }

    #[test]
    fn delete_drops_everything() {
        let mut map = FileIntentMap::new();
        map.set("src/foo.rs".into(), FileIntent::Delete);

        let mut fixes = vec![
            make_fix("src/foo.rs", InsertionKind::ImportAdd),
            make_fix("src/foo.rs", InsertionKind::MethodStub),
        ];
        let dropped = map.resolve_conflicts(&mut fixes);
        assert_eq!(dropped, 2);
        assert_eq!(fixes.len(), 0);
    }

    #[test]
    fn higher_priority_intent_wins() {
        let mut map = FileIntentMap::new();
        map.set("src/foo.rs".into(), FileIntent::Decompose);
        map.set("src/foo.rs".into(), FileIntent::Delete);
        assert_eq!(*map.get("src/foo.rs"), FileIntent::Delete);
    }

    #[test]
    fn lower_priority_does_not_overwrite() {
        let mut map = FileIntentMap::new();
        map.set("src/foo.rs".into(), FileIntent::Delete);
        map.set("src/foo.rs".into(), FileIntent::Decompose);
        assert_eq!(*map.get("src/foo.rs"), FileIntent::Delete);
    }

    #[test]
    fn has_structural_intent_checks_correctly() {
        let mut map = FileIntentMap::new();
        map.set("src/foo.rs".into(), FileIntent::Decompose);
        assert!(map.has_structural_intent("src/foo.rs"));
        assert!(!map.has_structural_intent("src/bar.rs"));
    }
}
