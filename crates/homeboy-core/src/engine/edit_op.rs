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
//! ## Layering
//!
//! This module holds only the engine-level primitives: the `EditOp` and
//! `InsertAnchor` vocabulary plus (in `edit_op_apply`) the filesystem apply
//! path. It deliberately depends on **nothing** from the fixer/refactor or
//! audit layers, so it stays at the bottom of the dependency graph.
//!
//! The tagged wrapper (`TaggedEditOp`) and all conversions from fixer/refactor
//! output (`Insertion`, `Fix`, `PropagateEdit`, `TransformMatch`, `FileRename`)
//! into edit ops live in `crate::refactor::edit_op_tagged` — those types belong
//! to the refactor layer and carry `RefactorPrimitive` / `AuditFinding` tags.
//! `apply_edit_ops()` takes plain `EditOp`s; refactor callers pass `&t.op`.

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
    /// Insert at a specific 1-indexed line number.
    ///
    /// Used by manual commands like `propagate` that compute exact
    /// insertion points from structural analysis.
    AtLine {
        /// 1-indexed line number to insert before.
        line: usize,
    },
}
