//! types — extracted from mod.rs.

use std::path::Path;
use crate::{component, is_zero, Result};


/// Summary counts for the audit report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditSummary {
    pub files_scanned: usize,
    pub conventions_detected: usize,
    #[serde(skip_serializing_if = "is_zero", default)]
    pub outliers_found: usize,
    /// Overall alignment score (0.0 = total chaos, 1.0 = perfect consistency).
    /// Null when no files could be fingerprinted (score would be meaningless).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub alignment_score: Option<f32>,
    /// Source files found but not fingerprinted (no extension provides fingerprinting).
    #[serde(skip_serializing_if = "is_zero", default)]
    pub files_skipped: usize,
    /// Warnings about the audit (e.g., unsupported file types).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Complete result of auditing a component's code conventions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeAuditResult {
    pub component_id: String,
    pub source_path: String,
    pub summary: AuditSummary,
    pub conventions: Vec<ConventionReport>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub directory_conventions: Vec<DirectoryConvention>,
    pub findings: Vec<Finding>,
    /// Grouped duplications for the fixer — each group has a canonical file and removal targets.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub duplicate_groups: Vec<duplication::DuplicateGroup>,
}

/// A cross-directory convention: a pattern that sibling subdirectories share.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectoryConvention {
    /// Parent directory path (e.g., "inc/Abilities").
    pub parent: String,
    /// Expected methods that most subdirectories' conventions share.
    pub expected_methods: Vec<String>,
    /// Expected registrations that most subdirectories share.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_registrations: Vec<String>,
    /// Subdirectories that conform.
    pub conforming_dirs: Vec<String>,
    /// Subdirectories that deviate.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub outlier_dirs: Vec<DirectoryOutlier>,
    /// How many subdirectories were analyzed.
    pub total_dirs: usize,
    /// Confidence score.
    pub confidence: f32,
}

/// A subdirectory that deviates from the cross-directory convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectoryOutlier {
    /// Subdirectory name.
    pub dir: String,
    /// What's missing compared to sibling conventions.
    pub missing_methods: Vec<String>,
    /// Missing registrations.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub missing_registrations: Vec<String>,
}

/// A convention as reported to the user (includes check status).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConventionReport {
    pub name: String,
    pub glob: String,
    pub status: CheckStatus,
    pub expected_methods: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_registrations: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_interfaces: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_namespace: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_imports: Vec<String>,
    pub conforming: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub outliers: Vec<Outlier>,
    pub total_files: usize,
    pub confidence: f32,
}
