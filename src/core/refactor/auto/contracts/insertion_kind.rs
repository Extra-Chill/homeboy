//! insertion_kind — extracted from contracts.rs.

use crate::code_audit::conventions::AuditFinding;
use std::path::Path;


#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionKind {
    MethodStub,
    RegistrationStub,
    ConstructorWithRegistration,
    /// Add a missing import/use statement at the top of the file.
    ImportAdd,
    /// Add a missing type conformance declaration to the primary type.
    /// Examples: `implements Foo`, `impl Foo for Bar`, `class X implements Foo`.
    TypeConformance,
    /// Add or replace a namespace declaration at the top of the file.
    NamespaceDeclaration,
    /// Remove a function definition (lines start_line..=end_line) and replace with an import.
    FunctionRemoval {
        /// 1-indexed start line (includes doc comments and attributes).
        start_line: usize,
        /// 1-indexed end line (inclusive).
        end_line: usize,
    },
    /// Insert a trait `use` statement inside a class body (PHP `use TraitName;`).
    /// Language-agnostic: for Rust this could be a trait impl, for JS a mixin.
    /// The code is inserted after the class/struct opening brace.
    TraitUse,
    /// Replace visibility qualifier on a specific line.
    /// `line` is 1-indexed. `from` is the old text, `to` is the replacement.
    VisibilityChange {
        /// 1-indexed line number where the change should be applied.
        line: usize,
        /// Text to find on that line (e.g., "pub fn").
        from: String,
        /// Replacement text (e.g., "pub(crate) fn").
        to: String,
    },
    /// Remove a function name from a `pub use { ... }` re-export block.
    /// Used when narrowing visibility of unreferenced exports that are
    /// also re-exported in parent mod.rs files.
    ReexportRemoval {
        /// The function name to remove from the re-export.
        fn_name: String,
    },
    /// Replace a stale path reference in a documentation file.
    DocReferenceUpdate {
        /// 1-indexed line number where the reference appears.
        line: usize,
        /// The old path text to find (e.g., "src/old/config.rs").
        old_ref: String,
        /// The new path text to replace with (e.g., "src/new/config.rs").
        new_ref: String,
    },
    /// Remove a full documentation line containing a dead reference.
    DocLineRemoval {
        /// 1-indexed line number to remove.
        line: usize,
    },
    /// Generic line-level text replacement.
    /// Finds `old_text` on the specified line and replaces with `new_text`.
    /// Used for test method renames and similar targeted edits.
    LineReplacement {
        /// 1-indexed line number where the replacement should be applied.
        line: usize,
        /// Text to find on that line.
        old_text: String,
        /// Replacement text.
        new_text: String,
    },
    /// Move a file to a new path. Creates parent directories as needed.
    /// Used for relocating misplaced test files to match source structure.
    FileMove {
        /// Current file path (relative to root).
        from: String,
        /// Target file path (relative to root).
        to: String,
    },
    /// Append an inline test module to the end of a source file.
    /// For Rust: `#[cfg(test)] mod tests { ... }`.
    /// For PHP: test class or method block.
    /// The code is appended after the last line of the file.
    TestModule,
}
