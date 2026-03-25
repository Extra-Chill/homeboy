//! fix_result — extracted from contracts.rs.

use crate::code_audit::conventions::AuditFinding;
use std::path::Path;
use super::Fix;
use super::NewFile;
use super::SkippedFile;
use super::ApplyChunkResult;
use super::DecomposeFixPlan;


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
