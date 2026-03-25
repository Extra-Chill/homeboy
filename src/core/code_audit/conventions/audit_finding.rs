//! audit_finding — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;
use super::super::fingerprint::FileFingerprint;


#[serde(rename_all = "snake_case")]
pub enum AuditFinding {
    MissingMethod,
    ExtraMethod,
    MissingRegistration,
    DifferentRegistration,
    MissingInterface,
    NamingMismatch,
    SignatureMismatch,
    NamespaceMismatch,
    MissingImport,
    /// File exceeds line count threshold.
    GodFile,
    /// File has too many top-level items.
    HighItemCount,
    /// Directory has too many source files in a flat namespace.
    DirectorySprawl,
    /// Function body is duplicated across files.
    DuplicateFunction,
    /// Function has identical structure but different identifiers/literals.
    NearDuplicate,
    /// Function parameter is declared but never used in the function body.
    /// When call-site data is available, this means no callers pass a value
    /// for this position — truly dead, safe to remove.
    UnusedParameter,
    /// Function parameter is received but ignored — callers ARE passing values
    /// for this position, but the function doesn't use them. Higher severity
    /// than UnusedParameter: likely a bug or stale param from a refactor.
    IgnoredParameter,
    /// Developer has marked code with a dead code suppression attribute.
    DeadCodeMarker,
    /// Public function/method is never imported or called by any other file.
    UnreferencedExport,
    /// Private/internal function is never called within the same file.
    OrphanedInternal,
    /// Source file has no corresponding test file.
    MissingTestFile,
    /// Source method/function has no corresponding test method.
    MissingTestMethod,
    /// Test file or test method has no corresponding source file/method.
    OrphanedTest,
    /// Comment starts with TODO/FIXME/HACK/XXX marker.
    TodoMarker,
    /// Comment starts with stale or legacy phrasing.
    LegacyComment,
    /// File violates a configured architecture/layer ownership rule.
    LayerOwnershipViolation,
    /// Inline test modules are present in source files instead of centralized tests.
    InlineTestModule,
    /// Test files are placed under source directories instead of the central tests tree.
    ScatteredTestFile,
    /// Duplicated code block found within the same method/function body.
    IntraMethodDuplicate,
    /// Two functions in different files follow the same call pattern —
    /// they invoke a parallel sequence of helpers, suggesting the shared
    /// workflow should be abstracted into a single parameterized function.
    ParallelImplementation,
    /// Documentation references a file, directory, or class that no longer exists.
    BrokenDocReference,
    /// Source feature (struct, trait, function, hook) has no mention in any docs.
    UndocumentedFeature,
    /// Documentation exists but references stale paths that have moved.
    StaleDocReference,
    /// Compiler warning (dead code, unused import, unused variable, etc).
    /// Detected by running the language compiler/checker (cargo check, tsc, etc).
    CompilerWarning,
    /// Wrapper file is missing an explicit declaration of what it wraps.
    /// Detected by tracing calls in the wrapper to infer the implementation target.
    MissingWrapperDeclaration,
    /// Two directories contain overlapping file names with high content similarity.
    /// Indicates a copy-paste module that was never consolidated.
    ShadowModule,
    /// Multiple structs define the same field group — candidates for extraction
    /// into a shared type and flattening/embedding.
    RepeatedFieldPattern,
}
