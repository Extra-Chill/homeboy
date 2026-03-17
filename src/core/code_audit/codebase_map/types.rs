//! types — extracted from codebase_map.rs.

use serde::Serialize;
use crate::{component, extension, Error};


/// A module in the codebase map — a group of related files in a directory.
#[derive(Serialize)]
pub struct MapModule {
    /// Human-readable module name (e.g., "REST API Controllers").
    pub name: String,
    /// Directory path relative to component root.
    pub path: String,
    /// Number of source files.
    pub file_count: usize,
    /// Classes/types found in this module.
    pub classes: Vec<MapClass>,
    /// Methods shared across most files (convention pattern).
    pub shared_methods: Vec<String>,
}

/// A class entry in the codebase map.
#[derive(Serialize)]
pub struct MapClass {
    /// Class/type name.
    pub name: String,
    /// File path relative to component root.
    pub file: String,
    /// Parent class name, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// Interfaces and traits.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    /// Namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Public methods.
    pub public_methods: Vec<String>,
    /// Protected methods (only if include_private).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub protected_methods: Vec<String>,
    /// Public/protected properties.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<String>,
    /// Hook references (actions and filters).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<extension::HookRef>,
}

/// The class hierarchy: parent → children mapping.
#[derive(Serialize)]
pub struct HierarchyEntry {
    pub parent: String,
    pub children: Vec<String>,
}

/// Summary of hooks in the codebase.
#[derive(Serialize)]
pub struct HookSummary {
    pub total_actions: usize,
    pub total_filters: usize,
    /// Top hook prefixes (e.g., "woocommerce_" → 847).
    pub top_prefixes: Vec<(String, usize)>,
}

/// Full codebase map output.
#[derive(Serialize)]
pub struct CodebaseMap {
    pub component: String,
    pub modules: Vec<MapModule>,
    pub class_hierarchy: Vec<HierarchyEntry>,
    pub hook_summary: HookSummary,
    pub total_files: usize,
    pub total_classes: usize,
}

/// Configuration for building a codebase map.
pub struct MapConfig<'a> {
    pub component_id: &'a str,
    /// Explicit source directories to scan. If `None`, auto-detect.
    pub source_dirs: Option<Vec<String>>,
    /// Include protected methods in the output.
    pub include_private: bool,
}

/// Maximum classes in a single module doc before we split it.
pub(crate) const MODULE_SPLIT_THRESHOLD: usize = 30;
