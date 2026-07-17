//! Tagged edit operations — the refactor layer's adapter onto engine `EditOp`s.
//!
//! The engine owns the pure `EditOp` / `InsertAnchor` vocabulary and the
//! filesystem apply path (`homeboy_core::engine::edit_op`, `edit_op_apply`). This
//! module owns everything that carries *refactor/audit* meaning:
//!
//! - `TaggedEditOp` — an `EditOp` plus its originating `RefactorPrimitive`
//!   and `AuditFinding` tags, description, and review flag.
//! - Conversions from fixer/refactor pipeline output into tagged ops
//!   (`Insertion`, `Fix`, `NewFile`, `PropagateEdit`, `TransformMatch`,
//!   `FileRename`).
//!
//! Keeping these here (not in `engine`) preserves the dependency direction:
//! the refactor layer depends on the engine primitives, never the reverse.
//! Callers apply tagged ops by passing the inner `&t.op` to
//! `homeboy_core::engine::edit_op_apply::apply_edit_ops`.

use homeboy_core::engine::edit_op::{EditOp, InsertAnchor};
use crate::auto::{Insertion, InsertionKind, RefactorPrimitive};
use homeboy_audit_contract::AuditFinding;

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
pub fn fix_to_edit_ops(fix: &crate::auto::Fix) -> Vec<TaggedEditOp> {
    fix.insertions
        .iter()
        .map(|ins| from_insertion(ins, &fix.file))
        .collect()
}

/// Translate a `NewFile` into a `TaggedEditOp`.
pub fn new_file_to_edit_op(nf: &crate::auto::NewFile) -> TaggedEditOp {
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
pub fn fix_result_to_edit_ops(result: &crate::auto::FixResult) -> Vec<TaggedEditOp> {
    let mut ops: Vec<TaggedEditOp> = result.fixes.iter().flat_map(fix_to_edit_ops).collect();

    for nf in &result.new_files {
        ops.push(new_file_to_edit_op(nf));
    }

    ops
}

// ============================================================================
// Manual command conversions
// ============================================================================

/// Translate a `PropagateEdit` into a `TaggedEditOp`.
pub fn propagate_edit_to_edit_op(edit: &crate::propagate::PropagateEdit) -> TaggedEditOp {
    TaggedEditOp {
        op: EditOp::InsertLines {
            file: edit.file.clone(),
            anchor: InsertAnchor::AtLine { line: edit.line },
            code: edit.insert_text.clone(),
        },
        primitive: None,
        finding: None,
        description: edit.description.clone(),
        manual_only: false,
    }
}

/// Translate a `PropagateResult` into a list of `TaggedEditOp`s.
pub fn propagate_result_to_edit_ops(
    result: &crate::propagate::PropagateResult,
) -> Vec<TaggedEditOp> {
    result.edits.iter().map(propagate_edit_to_edit_op).collect()
}

/// Translate a `TransformMatch` into a `TaggedEditOp`.
pub fn transform_match_to_edit_op(m: &crate::transform::TransformMatch) -> TaggedEditOp {
    TaggedEditOp {
        op: EditOp::ReplaceText {
            file: m.file.clone(),
            line: m.line,
            old_text: m.before.clone(),
            new_text: m.after.clone(),
        },
        primitive: None,
        finding: None,
        description: format!("Transform: {} → {}", m.before, m.after),
        manual_only: false,
    }
}

/// Translate a `TransformResult` into a list of `TaggedEditOp`s.
pub fn transform_result_to_edit_ops(
    result: &crate::transform::TransformResult,
) -> Vec<TaggedEditOp> {
    result
        .rules
        .iter()
        .flat_map(|rule| {
            rule.matches.iter().map(|m| {
                let mut op = transform_match_to_edit_op(m);
                op.description = format!("{}: {}", rule.description, op.description);
                op
            })
        })
        .collect()
}

// ============================================================================
// Rename command conversions
// ============================================================================

/// Translate a `FileRename` into a `TaggedEditOp`.
pub fn file_rename_to_edit_op(rename: &crate::FileRename) -> TaggedEditOp {
    TaggedEditOp {
        op: EditOp::MoveFile {
            from: rename.from.clone(),
            to: rename.to.clone(),
        },
        primitive: None,
        finding: None,
        description: format!("Rename: {} → {}", rename.from, rename.to),
        manual_only: false,
    }
}

/// Translate the file renames from a `RenameResult` into `TaggedEditOp`s.
///
/// Only converts file/directory renames (→ `MoveFile`), not content edits.
/// Content edits operate at whole-file granularity and are applied directly
/// by the rename engine.
pub fn rename_file_moves_to_edit_ops(result: &crate::RenameResult) -> Vec<TaggedEditOp> {
    result
        .file_renames
        .iter()
        .map(file_rename_to_edit_op)
        .collect()
}

// Apply logic lives in `edit_op_apply` — see that module for:
// resolve_anchor(), apply_edit_ops_to_content(), apply_edit_ops().

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto::{Fix, Insertion, InsertionKind, RefactorPrimitive};
    use homeboy_audit_contract::AuditFinding;

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
