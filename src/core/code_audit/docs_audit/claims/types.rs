//! types — extracted from claims.rs.

use regex::Regex;
use std::sync::LazyLock;


/// Types of claims that can be extracted from documentation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    /// File path reference (e.g., `src/main.rs`, `/inc/foo/bar.php`)
    FilePath,
    /// Directory path reference (e.g., `src/core/`, `/inc/Engine/`)
    DirectoryPath,
    /// Code example in a fenced block
    CodeExample,
    /// Namespaced class reference (e.g., `DataMachine\Services\CacheManager`)
    ClassName,
}

/// How confident we are that a claim is a real reference vs. a placeholder/example.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimConfidence {
    /// Real reference — expected to resolve against codebase
    Real,
    /// Likely a placeholder or example (inside code block, generic names)
    Example,
    /// Cannot determine — needs manual review
    Unclear,
}

/// A claim extracted from documentation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Claim {
    pub claim_type: ClaimType,
    pub value: String,
    pub doc_file: String,
    pub line: usize,
    pub confidence: ClaimConfidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}
