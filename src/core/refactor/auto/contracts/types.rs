//! types — extracted from contracts.rs.

use crate::code_audit::conventions::AuditFinding;
use crate::core::refactor::decompose;
use std::path::Path;
use super::InsertionKind;


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

/// A decompose operation generated from a GodFile or HighItemCount finding.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecomposeFixPlan {
    pub file: String,
    pub plan: decompose::DecomposePlan,
    pub source_finding: AuditFinding,
    #[serde(default)]
    pub applied: bool,
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
