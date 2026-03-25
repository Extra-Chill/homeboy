//! language — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;
use super::super::fingerprint::FileFingerprint;


#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Php,
    Rust,
    JavaScript,
    TypeScript,
    #[default]
    Unknown,
}
