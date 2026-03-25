//! Codebase map — structural analysis that builds a navigable map of modules,
//! classes, hooks, and class hierarchies from source code fingerprints.
//!
//! The map is a read-only reference: it scans an entire component's source tree
//! using [`code_audit::fingerprint`] and groups results by directory into modules.
//! Output is either a [`CodebaseMap`] JSON structure or rendered markdown files.

mod helpers;
mod markdown_rendering;
mod source_directory_detection;
mod types;

pub use helpers::*;
pub use markdown_rendering::*;
pub use source_directory_detection::*;
pub use types::*;


use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::code_audit::fingerprint::{self, FileFingerprint};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::{component, extension, Error};

// ============================================================================
// Types
// ============================================================================

// ============================================================================
// Map builder
// ============================================================================

// ============================================================================
// Markdown rendering
// ============================================================================

// ============================================================================
// Helpers
// ============================================================================

    let threshold = (classes.len() as f64 * 0.5).ceil() as usize;
    let mut best = String::new();
    for (prefix, count) in &prefix_counts {
        if *count >= threshold && prefix.len() > best.len() {
            best = prefix.to_string();
        }
    }

    best
}

// ============================================================================
// Source directory detection
// ============================================================================
