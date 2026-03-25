//! types — extracted from baseline.rs.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use serde_json::Value;
use crate::error::{Error, Result};


pub trait Fingerprintable {
    fn fingerprint(&self) -> String;
    fn description(&self) -> String;
    fn context_label(&self) -> String;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline<M: Serialize> {
    pub created_at: String,
    pub context_id: String,
    pub item_count: usize,
    pub known_fingerprints: Vec<String>,
    pub metadata: M,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub new_items: Vec<NewItem>,
    pub resolved_fingerprints: Vec<String>,
    pub delta: i64,
    pub drift_increased: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewItem {
    pub fingerprint: String,
    pub description: String,
    pub context_label: String,
}
