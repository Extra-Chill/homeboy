use crate::code_audit::conventions::AuditFinding;
use crate::core::refactor::decompose;
use std::path::Path;

/// Callback that verifies an applied chunk, returning Ok(message) or Err(reason).
pub type ChunkVerifier<'a> = &'a dyn Fn(&ApplyChunkResult) -> Result<String, String>;

/// A planned fix for a single file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fix {
    /// Relative path to the file being fixed.
    pub file: String,
    /// Expected methods that should still be present after applying this fix.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub required_methods: Vec<String>,
    /// Expected registration calls that should still be present after applying this fix.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub required_registrations: Vec<String>,
    /// What will be inserted.
    pub insertions: Vec<Insertion>,
    /// Whether the fix was applied to disk.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub applied: bool,
}

/// A single insertion into a file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Insertion {
    /// What kind of fix (mechanical action).
    pub kind: InsertionKind,
    /// The audit finding this insertion addresses.
    pub finding: AuditFinding,
    /// Safety contract for this insertion.
    pub safety_tier: FixSafetyTier,
    /// Whether this fix is eligible for auto-apply under the current policy.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub auto_apply: bool,
    /// Why the fix is not auto-applied under the current policy.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blocked_reason: Option<String>,
    /// Deterministic preflight validation report for safe_with_checks writes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preflight: Option<PreflightReport>,
    /// The code to insert.
    pub code: String,
    /// Human-readable description.
    pub description: String,
}

/// Safety classification for automated code fixes.
///
/// Two tiers: `Safe` fixes are auto-applied (with preflight validation when applicable).
/// `PlanOnly` fixes are preview-only and require human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FixSafetyTier {
    /// Fix can be auto-applied. Preflight validation runs when applicable.
    #[serde(
        rename = "safe",
        alias = "safe_auto",
        alias = "safe_with_checks",
        alias = "Safe",
        alias = "SafeAuto",
        alias = "SafeWithChecks"
    )]
    Safe,
    /// Fix requires human review — never auto-applied.
    #[serde(rename = "plan_only", alias = "PlanOnly")]
    PlanOnly,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreflightReport {
    pub status: PreflightStatus,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub checks: Vec<PreflightCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightStatus {
    Passed,
    Failed,
    NotApplicable,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreflightCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

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
}

impl InsertionKind {
    pub fn safety_tier(&self) -> FixSafetyTier {
        match self {
            // Safe: all deterministic, mechanical fixes that can be auto-applied.
            // Preflight validation runs when applicable (registration stubs get
            // collision checks, visibility changes get simulation checks, etc).
            Self::ImportAdd
            | Self::DocReferenceUpdate { .. }
            | Self::DocLineRemoval { .. }
            | Self::RegistrationStub
            | Self::ConstructorWithRegistration
            | Self::TypeConformance
            | Self::NamespaceDeclaration
            | Self::VisibilityChange { .. }
            | Self::ReexportRemoval { .. }
            | Self::LineReplacement { .. } => FixSafetyTier::Safe,

            // Plan-only: requires human review.
            Self::MethodStub | Self::FunctionRemoval { .. } | Self::TraitUse => {
                FixSafetyTier::PlanOnly
            }
        }
    }
}

/// A file that was skipped by the fixer with a reason.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkippedFile {
    /// Relative file path.
    pub file: String,
    /// Why it was skipped.
    pub reason: String,
}

/// A new file to create (e.g., a trait file for extracted duplicates).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewFile {
    /// Relative path for the new file.
    pub file: String,
    /// The audit finding this new file addresses.
    pub finding: AuditFinding,
    /// Safety contract for this file creation.
    pub safety_tier: FixSafetyTier,
    /// Whether this file is eligible for auto-apply under the current policy.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub auto_apply: bool,
    /// Why this file is not auto-applied under the current policy.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blocked_reason: Option<String>,
    /// Deterministic preflight validation report for safe_with_checks writes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preflight: Option<PreflightReport>,
    /// Content to write.
    pub content: String,
    /// Human-readable description.
    pub description: String,
    /// Whether the file was written to disk.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub written: bool,
}

/// Result of running the fixer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FixResult {
    pub fixes: Vec<Fix>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub new_files: Vec<NewFile>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub decompose_plans: Vec<DecomposeFixPlan>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub skipped: Vec<SkippedFile>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub chunk_results: Vec<ApplyChunkResult>,
    pub total_insertions: usize,
    pub files_modified: usize,
}

/// A decompose operation generated from a GodFile finding.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecomposeFixPlan {
    pub file: String,
    pub plan: decompose::DecomposePlan,
    #[serde(default)]
    pub applied: bool,
}

impl FixResult {
    /// Strip generated code from insertions and new files, replacing with byte-count placeholders.
    pub fn strip_code(&mut self) {
        for fix in &mut self.fixes {
            for insertion in &mut fix.insertions {
                let len = insertion.code.len();
                insertion.code = format!("[{len} bytes]");
            }
        }
        for new_file in &mut self.new_files {
            let len = new_file.content.len();
            new_file.content = format!("[{len} bytes]");
        }
    }

    /// Compute a breakdown of finding types and their fix counts.
    pub fn finding_counts(&self) -> std::collections::BTreeMap<AuditFinding, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for fix in &self.fixes {
            for insertion in &fix.insertions {
                *counts.entry(insertion.finding.clone()).or_insert(0) += 1;
            }
        }
        for new_file in &self.new_files {
            *counts.entry(new_file.finding.clone()).or_insert(0) += 1;
        }
        if !self.decompose_plans.is_empty() {
            *counts.entry(AuditFinding::GodFile).or_insert(0) += self.decompose_plans.len();
        }
        counts
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApplyChunkResult {
    pub chunk_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files: Vec<String>,
    pub status: ChunkStatus,
    pub applied_files: usize,
    #[serde(skip_serializing_if = "is_zero_usize", default)]
    pub reverted_files: usize,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStatus {
    Applied,
    Reverted,
}

#[derive(Clone)]
pub struct ApplyOptions<'a> {
    pub verifier: Option<ChunkVerifier<'a>>,
}

#[derive(Debug, Clone, Default)]
pub struct FixPolicy {
    pub only: Option<Vec<AuditFinding>>,
    pub exclude: Vec<AuditFinding>,
}

#[derive(Debug, Clone)]
pub struct PreflightContext<'a> {
    pub root: &'a Path,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicySummary {
    pub visible_insertions: usize,
    pub visible_new_files: usize,
    pub auto_apply_insertions: usize,
    pub auto_apply_new_files: usize,
    pub blocked_insertions: usize,
    pub blocked_new_files: usize,
    pub preflight_failures: usize,
}

impl PolicySummary {
    pub fn has_blocked_items(&self) -> bool {
        self.blocked_insertions > 0 || self.blocked_new_files > 0
    }
}

pub(crate) fn insertion(
    kind: InsertionKind,
    finding: AuditFinding,
    code: String,
    description: String,
) -> Insertion {
    Insertion {
        safety_tier: kind.safety_tier(),
        kind,
        finding,
        auto_apply: false,
        blocked_reason: None,
        preflight: None,
        code,
        description,
    }
}

pub(crate) fn new_file(
    finding: AuditFinding,
    safety_tier: FixSafetyTier,
    file: String,
    content: String,
    description: String,
) -> NewFile {
    NewFile {
        file,
        finding,
        safety_tier,
        auto_apply: false,
        blocked_reason: None,
        preflight: None,
        content,
        description,
        written: false,
    }
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}
