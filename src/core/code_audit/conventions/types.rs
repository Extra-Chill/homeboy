//! types — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;
use super::super::fingerprint::FileFingerprint;
use super::AuditFinding;
use super::std::str::FromStr for AuditFinding;


/// A discovered convention: a pattern that most files in a group follow.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Convention {
    /// Human-readable name (auto-generated or from config).
    pub name: String,
    /// The glob pattern that groups these files.
    pub glob: String,
    /// The expected methods/functions that define the convention.
    pub expected_methods: Vec<String>,
    /// The expected registration calls.
    pub expected_registrations: Vec<String>,
    /// The expected interfaces/traits that files should implement.
    pub expected_interfaces: Vec<String>,
    /// The expected namespace pattern (if consistent across files).
    pub expected_namespace: Option<String>,
    /// The expected import/use statements.
    pub expected_imports: Vec<String>,
    /// Files that follow the convention.
    pub conforming: Vec<String>,
    /// Files that deviate from the convention.
    pub outliers: Vec<Outlier>,
    /// How many files were analyzed.
    pub total_files: usize,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f32,
}

/// A file that deviates from a convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Outlier {
    /// Relative file path.
    pub file: String,
    /// Whether this outlier appears to be helper/utility drift rather than a real member.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub noisy: bool,
    /// What's missing or different.
    pub deviations: Vec<Deviation>,
}

/// A specific deviation from the convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Deviation {
    /// What kind of deviation.
    pub kind: AuditFinding,
    /// Human-readable description.
    pub description: String,
    /// Suggested fix.
    pub suggestion: String,
}

impl std::str::FromStr for AuditFinding {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        let json = format!("\"{}\"", normalized);
        serde_json::from_str(&json).map_err(|_| {
            format!(
                "unknown finding kind '{}'. Valid kinds: {}",
                value,
                Self::all_names().join(", ")
            )
        })
    }
}
