use serde::{Deserialize, Serialize};

use super::extend_unique;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct KnownSymbolsConfig {
    /// Header-version providers keyed by an extension-owned marker and header.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_versions: Vec<KnownSymbolHeaderVersionProvider>,
    /// Dependency-manifest package providers keyed by manifest + package name.
    ///
    /// Each provider names the dependency manifest file (an extension may point
    /// at its ecosystem's manifest) and the keys within it that hold declared
    /// package names. Core does no ecosystem-specific parsing of its own — it
    /// only inspects manifests an extension explicitly declares.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest_packages: Vec<KnownSymbolManifestPackageProvider>,
    /// Bootstrap path providers keyed by a normalized path substring or suffix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_paths: Vec<KnownSymbolBootstrapPathProvider>,
    /// Extension-owned source scanning contract for entry-file discovery and
    /// unconditional include/require parsing. When unset, core performs no
    /// ecosystem-specific entry-file scanning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_scan: Option<KnownSymbolSourceScanConfig>,
}

/// Extension-owned contract describing how to discover entry files and parse
/// unconditional include/require statements from them. All literals here are
/// supplied by the owning language extension so core stays ecosystem-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct KnownSymbolSourceScanConfig {
    /// File extensions (without the leading dot) that identify candidate entry
    /// files (an extension supplies its ecosystem's source extensions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_file_extensions: Vec<String>,
    /// Statement prefixes that introduce an unconditional include/require,
    /// e.g. `["require_once", "require", "include_once", "include"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_keywords: Vec<String>,
    /// Guard-call markers that, when present on the opening line of an enclosing
    /// `if` block, mean a following require is conditional and must be skipped,
    /// e.g. `["class_exists", "function_exists", "defined"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guard_markers: Vec<String>,
}

impl KnownSymbolSourceScanConfig {
    pub fn is_empty(&self) -> bool {
        self.entry_file_extensions.is_empty()
            && self.require_keywords.is_empty()
            && self.guard_markers.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolHeaderVersionProvider {
    /// Marker used to locate the component entry file.
    pub file_marker: String,
    /// Header key whose value contains the runtime version floor.
    pub version_header: String,
    /// Symbols introduced by runtime version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<KnownSymbolVersionedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolManifestPackageProvider {
    /// Dependency manifest file name relative to the component root (an
    /// extension declares its ecosystem's manifest file name).
    pub manifest_file: String,
    /// JSON object keys within the manifest that map package name -> version,
    /// e.g. `["require", "require-dev"]` or `["dependencies", "devDependencies"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_keys: Vec<String>,
    /// Package name that, when declared in the manifest, guarantees `symbols`.
    pub package: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<KnownSymbolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolBootstrapPathProvider {
    pub path_contains: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<KnownSymbolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolVersionedEntry {
    pub name: String,
    pub kind: KnownSymbolKind,
    pub introduced: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolEntry {
    pub name: String,
    pub kind: KnownSymbolKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KnownSymbolKind {
    Function,
    Class,
    Constant,
}

impl KnownSymbolsConfig {
    pub fn is_empty(&self) -> bool {
        self.header_versions.is_empty()
            && self.manifest_packages.is_empty()
            && self.bootstrap_paths.is_empty()
            && self
                .source_scan
                .as_ref()
                .map(KnownSymbolSourceScanConfig::is_empty)
                .unwrap_or(true)
    }

    pub(super) fn merge(&mut self, other: &KnownSymbolsConfig) {
        extend_unique(&mut self.header_versions, &other.header_versions);
        extend_unique(&mut self.manifest_packages, &other.manifest_packages);
        extend_unique(&mut self.bootstrap_paths, &other.bootstrap_paths);
        if self.source_scan.is_none() {
            self.source_scan = other.source_scan.clone();
        }
    }
}
