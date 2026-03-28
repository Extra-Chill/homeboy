//! Shared edit operations — the canonical vocabulary for file modifications.
//!
//! Both the autofix fixer pipeline and manual refactor commands (rename,
//! transform, move, decompose) perform the same handful of mechanical
//! operations on files. `EditOp` is the shared type that captures these.
//!
//! ## Design
//!
//! Five atomic operations cover all current edit patterns:
//!
//! - `ReplaceText` — find-and-replace on a single line
//! - `RemoveLines` — delete a contiguous range of lines
//! - `InsertLines` — add code at a position (import, stub, etc.)
//! - `MoveFile` — rename/relocate a file
//! - `CreateFile` — write a new file from scratch
//!
//! Each operation carries an optional `RefactorPrimitive` tag so CI
//! summaries, undo, and debugging all speak the same vocabulary regardless
//! of whether the edit came from a fixer or a manual command.
//!
//! ## Current integration
//!
//! The fixer pipeline still uses `InsertionKind` internally for apply logic.
//! `EditOp` provides a parallel representation via `from_insertion()` for
//! reporting and cross-system compatibility. Manual commands will produce
//! `EditOp` directly in a future phase.

use crate::code_audit::AuditFinding;
use crate::core::refactor::auto::{Insertion, InsertionKind, RefactorPrimitive};

/// Atomic file edit operation.
///
/// The shared vocabulary for all file modifications in the refactor engine.
/// Fixer pipelines and manual commands both reduce to these operations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    /// Find-and-replace text on a single line.
    ///
    /// Covers: `VisibilityChange`, `LineReplacement`, `DocReferenceUpdate`.
    ReplaceText {
        /// Relative file path.
        file: String,
        /// 1-indexed line number.
        line: usize,
        /// Text to find on that line.
        old_text: String,
        /// Replacement text.
        new_text: String,
    },

    /// Remove a contiguous range of lines (inclusive).
    ///
    /// Covers: `FunctionRemoval`, `DocLineRemoval`.
    RemoveLines {
        /// Relative file path.
        file: String,
        /// 1-indexed start line.
        start_line: usize,
        /// 1-indexed end line (inclusive).
        end_line: usize,
    },

    /// Insert code at a logical position in a file.
    ///
    /// Covers: `ImportAdd`, `MethodStub`, `RegistrationStub`,
    /// `ConstructorWithRegistration`, `TraitUse`, `TypeConformance`,
    /// `NamespaceDeclaration`, `TestModule`, `ReexportRemoval`.
    ///
    /// The `anchor` describes where to insert. The apply logic resolves
    /// the actual line number based on file content and language.
    InsertLines {
        /// Relative file path.
        file: String,
        /// Where in the file to insert.
        anchor: InsertAnchor,
        /// The code to insert.
        code: String,
    },

    /// Move a file to a new path.
    ///
    /// Covers: `FileMove`.
    MoveFile {
        /// Current relative path.
        from: String,
        /// Target relative path.
        to: String,
    },

    /// Create a new file with the given content.
    ///
    /// Covers: `NewFile` from the fixer pipeline.
    CreateFile {
        /// Relative file path to create.
        file: String,
        /// Full file content.
        content: String,
    },
}

/// Logical position for inserting code into a file.
///
/// The apply layer resolves these anchors to actual line numbers based
/// on file content and language rules.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertAnchor {
    /// After the last import/use statement.
    AfterImports,
    /// After the class/struct opening brace (for trait uses).
    AfterClassOpen,
    /// Inside the constructor body, after the opening brace.
    InConstructor,
    /// Before the last closing brace in the file (for method stubs).
    BeforeClosingBrace,
    /// Replace or insert at the top of the file (for namespace declarations).
    FileTop,
    /// Append to the end of the file (for test modules).
    FileEnd,
    /// Remove a symbol from a re-export block (structural edit).
    RemoveFromReexport {
        /// The symbol name to remove.
        symbol: String,
    },
    /// Add a type conformance to the primary type declaration.
    TypeDeclaration,
}

/// An `EditOp` with metadata about its origin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaggedEditOp {
    /// The edit operation.
    #[serde(flatten)]
    pub op: EditOp,
    /// The refactor primitive this operation represents, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primitive: Option<RefactorPrimitive>,
    /// The audit finding this operation addresses, if from the fixer pipeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding: Option<AuditFinding>,
    /// Human-readable description.
    pub description: String,
    /// Whether this operation requires manual review.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub manual_only: bool,
}

/// Translate a fixer pipeline `Insertion` into `EditOp`(s).
///
/// Most insertions map 1:1 to an EditOp. The `file` parameter is the
/// relative file path from the parent `Fix`.
pub fn from_insertion(insertion: &Insertion, file: &str) -> TaggedEditOp {
    let op = match &insertion.kind {
        InsertionKind::VisibilityChange { line, from, to } => EditOp::ReplaceText {
            file: file.to_string(),
            line: *line,
            old_text: from.clone(),
            new_text: to.clone(),
        },

        InsertionKind::LineReplacement {
            line,
            old_text,
            new_text,
        } => EditOp::ReplaceText {
            file: file.to_string(),
            line: *line,
            old_text: old_text.clone(),
            new_text: new_text.clone(),
        },

        InsertionKind::DocReferenceUpdate {
            line,
            old_ref,
            new_ref,
        } => EditOp::ReplaceText {
            file: file.to_string(),
            line: *line,
            old_text: old_ref.clone(),
            new_text: new_ref.clone(),
        },

        InsertionKind::FunctionRemoval {
            start_line,
            end_line,
        } => EditOp::RemoveLines {
            file: file.to_string(),
            start_line: *start_line,
            end_line: *end_line,
        },

        InsertionKind::DocLineRemoval { line } => EditOp::RemoveLines {
            file: file.to_string(),
            start_line: *line,
            end_line: *line,
        },

        InsertionKind::ImportAdd => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::AfterImports,
            code: insertion.code.clone(),
        },

        InsertionKind::TraitUse => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::AfterClassOpen,
            code: insertion.code.clone(),
        },

        InsertionKind::RegistrationStub => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::InConstructor,
            code: insertion.code.clone(),
        },

        InsertionKind::ConstructorWithRegistration => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::BeforeClosingBrace,
            code: insertion.code.clone(),
        },

        InsertionKind::MethodStub => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::BeforeClosingBrace,
            code: insertion.code.clone(),
        },

        InsertionKind::NamespaceDeclaration => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::FileTop,
            code: insertion.code.clone(),
        },

        InsertionKind::TypeConformance => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::TypeDeclaration,
            code: insertion.code.clone(),
        },

        InsertionKind::TestModule => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::FileEnd,
            code: insertion.code.clone(),
        },

        InsertionKind::ReexportRemoval { fn_name } => EditOp::InsertLines {
            file: file.to_string(),
            anchor: InsertAnchor::RemoveFromReexport {
                symbol: fn_name.clone(),
            },
            code: String::new(),
        },

        InsertionKind::FileMove { from, to } => EditOp::MoveFile {
            from: from.clone(),
            to: to.clone(),
        },
    };

    TaggedEditOp {
        op,
        primitive: insertion.primitive.clone(),
        finding: Some(insertion.finding.clone()),
        description: insertion.description.clone(),
        manual_only: insertion.manual_only,
    }
}

/// Translate an entire `Fix` into a list of `TaggedEditOp`s.
pub fn fix_to_edit_ops(fix: &crate::core::refactor::auto::Fix) -> Vec<TaggedEditOp> {
    fix.insertions
        .iter()
        .map(|ins| from_insertion(ins, &fix.file))
        .collect()
}

/// Translate a `NewFile` into a `TaggedEditOp`.
pub fn new_file_to_edit_op(nf: &crate::core::refactor::auto::NewFile) -> TaggedEditOp {
    TaggedEditOp {
        op: EditOp::CreateFile {
            file: nf.file.clone(),
            content: nf.content.clone(),
        },
        primitive: nf.primitive.clone(),
        finding: Some(nf.finding.clone()),
        description: nf.description.clone(),
        manual_only: nf.manual_only,
    }
}

/// Translate an entire `FixResult` into a flat list of `TaggedEditOp`s.
///
/// This is the primary reporting/debugging surface — it shows every edit
/// the refactor engine would perform, in a unified format.
pub fn fix_result_to_edit_ops(
    result: &crate::core::refactor::auto::FixResult,
) -> Vec<TaggedEditOp> {
    let mut ops: Vec<TaggedEditOp> = result.fixes.iter().flat_map(fix_to_edit_ops).collect();

    for nf in &result.new_files {
        ops.push(new_file_to_edit_op(nf));
    }

    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::AuditFinding;
    use crate::core::refactor::auto::{Fix, Insertion, InsertionKind, RefactorPrimitive};

    fn test_insertion(kind: InsertionKind) -> Insertion {
        Insertion {
            primitive: None,
            kind,
            finding: AuditFinding::UnreferencedExport,
            manual_only: false,
            auto_apply: false,
            blocked_reason: None,
            code: String::new(),
            description: "test".to_string(),
        }
    }

    #[test]
    fn visibility_change_maps_to_replace_text() {
        let ins = test_insertion(InsertionKind::VisibilityChange {
            line: 10,
            from: "pub fn".to_string(),
            to: "pub(crate) fn".to_string(),
        });
        let tagged = from_insertion(&ins, "src/lib.rs");
        assert!(matches!(tagged.op, EditOp::ReplaceText { line: 10, .. }));
    }

    #[test]
    fn line_replacement_maps_to_replace_text() {
        let ins = test_insertion(InsertionKind::LineReplacement {
            line: 5,
            old_text: "old_name".to_string(),
            new_text: "new_name".to_string(),
        });
        let tagged = from_insertion(&ins, "src/lib.rs");
        assert!(matches!(tagged.op, EditOp::ReplaceText { line: 5, .. }));
    }

    #[test]
    fn function_removal_maps_to_remove_lines() {
        let ins = test_insertion(InsertionKind::FunctionRemoval {
            start_line: 10,
            end_line: 20,
        });
        let tagged = from_insertion(&ins, "src/lib.rs");
        assert!(matches!(
            tagged.op,
            EditOp::RemoveLines {
                start_line: 10,
                end_line: 20,
                ..
            }
        ));
    }

    #[test]
    fn doc_line_removal_maps_to_remove_single_line() {
        let ins = test_insertion(InsertionKind::DocLineRemoval { line: 42 });
        let tagged = from_insertion(&ins, "docs/api.md");
        assert!(matches!(
            tagged.op,
            EditOp::RemoveLines {
                start_line: 42,
                end_line: 42,
                ..
            }
        ));
    }

    #[test]
    fn import_add_maps_to_insert_lines() {
        let mut ins = test_insertion(InsertionKind::ImportAdd);
        ins.code = "use crate::foo;".to_string();
        let tagged = from_insertion(&ins, "src/lib.rs");
        assert!(matches!(
            tagged.op,
            EditOp::InsertLines {
                anchor: InsertAnchor::AfterImports,
                ..
            }
        ));
    }

    #[test]
    fn file_move_maps_to_move_file() {
        let ins = test_insertion(InsertionKind::FileMove {
            from: "tests/old_test.rs".to_string(),
            to: "tests/new_test.rs".to_string(),
        });
        let tagged = from_insertion(&ins, "tests/old_test.rs");
        assert!(matches!(tagged.op, EditOp::MoveFile { .. }));
    }

    #[test]
    fn primitive_tag_is_preserved() {
        let mut ins = test_insertion(InsertionKind::FunctionRemoval {
            start_line: 1,
            end_line: 5,
        });
        ins.primitive = Some(RefactorPrimitive::RemoveOrphanedTest);
        let tagged = from_insertion(&ins, "src/lib.rs");
        assert_eq!(
            tagged.primitive,
            Some(RefactorPrimitive::RemoveOrphanedTest)
        );
    }

    #[test]
    fn manual_only_is_preserved() {
        let mut ins = test_insertion(InsertionKind::DocLineRemoval { line: 1 });
        ins.manual_only = true;
        let tagged = from_insertion(&ins, "src/lib.rs");
        assert!(tagged.manual_only);
    }

    #[test]
    fn fix_to_edit_ops_produces_one_per_insertion() {
        let fix = Fix {
            file: "src/lib.rs".to_string(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![
                test_insertion(InsertionKind::FunctionRemoval {
                    start_line: 1,
                    end_line: 5,
                }),
                test_insertion(InsertionKind::ImportAdd),
            ],
            applied: false,
        };
        let ops = fix_to_edit_ops(&fix);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0].op, EditOp::RemoveLines { .. }));
        assert!(matches!(ops[1].op, EditOp::InsertLines { .. }));
    }
}
