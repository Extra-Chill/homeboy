//! Declarative toolchain PATH schema for rig specs.
//!
//! Homeboy is a language- and product-agnostic orchestrator, so the set of bin
//! directories a rig's `command` steps should see belongs in configuration, not
//! in the generic rig crate. This module is the declarative form of that set.
//!
//! A rig that omits `toolchain` keeps Homeboy's built-in default
//! (`crate::toolchain::builtin_default_spec`), so existing rigs are unchanged.

use serde::{Deserialize, Serialize};

/// Declarative toolchain PATH assembly for rig `command` steps.
///
/// Resolution order, highest priority first:
///
/// 1. `prepend_paths`, in declared order
/// 2. `discover` results, in declared order (each scan emits its own matches in
///    its declared `sort` order)
/// 3. `append_paths`, in declared order
/// 4. the inherited process `PATH`
///
/// Every entry is variable-expanded (`~`, `${env.NAME}`,
/// `${components.<id>.path}`, `${package.root}`) and dropped if the directory
/// does not exist, so a spec stays portable across hosts. Duplicates are
/// removed, keeping the highest-priority occurrence.
///
/// `append_paths` exists because the built-in default interleaves literal
/// directories around discovery (home bin dirs, then version-manager scans,
/// then system bin dirs). Two flat lists could not express that order, and the
/// default must stay byte-for-byte identical.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainSpec {
    /// Directories placed ahead of everything else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepend_paths: Vec<String>,

    /// Version-manager style scans applied after `prepend_paths`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discover: Vec<PathDiscoverySpec>,

    /// Directories placed after `discover` but still ahead of the inherited
    /// `PATH`. Typically system or package-manager bin directories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append_paths: Vec<String>,
}

/// A version-manager style directory scan.
///
/// For every immediate child of `root` whose file name matches `glob` (all
/// children when `glob` is absent), the scan contributes `<child>/<bin_subdir>`
/// (or `<child>` itself when `bin_subdir` is absent). Missing roots and missing
/// bin directories are skipped silently.
///
/// The nvm layout, for example, is
/// `{ "root": "~/.nvm/versions/node", "glob": "v*", "bin_subdir": "bin", "sort": "descending" }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDiscoverySpec {
    /// Directory whose immediate children are scanned.
    pub root: String,

    /// Optional `*`-wildcard filter applied to each child's file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,

    /// Optional subdirectory appended to each matching child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_subdir: Option<String>,

    /// Order the matches are contributed in. Defaults to `descending`, which is
    /// the "newest version wins" behavior version managers imply.
    #[serde(default)]
    pub sort: PathDiscoverySort,
}

/// Ordering applied to a `PathDiscoverySpec`'s matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDiscoverySort {
    /// Lexicographic ascending.
    Ascending,
    /// Lexicographic descending — the default; newest version directory first.
    #[default]
    Descending,
    /// Whatever order the filesystem returns. Non-deterministic; only useful
    /// when exactly one match is expected.
    Unsorted,
}
