//! language — extracted from conventions.rs.

use super::super::fingerprint::FileFingerprint;
use super::super::*;
use std::collections::HashMap;
use std::path::Path;

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
