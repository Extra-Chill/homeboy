//! The extension source root manifest (`homeboy-extension-root.json`).
//!
//! Shared assets are declared by a source root rather than by extension
//! manifests, in either a bare-path or object form. Catalog listing, install
//! source resolution and runtime packaging all read that same declaration, so
//! the shape lives here once instead of being restated per consumer.

use serde::Deserialize;

/// Manifest at the root of an extension source tree.
#[derive(Debug, Deserialize)]
pub(crate) struct ExtensionRootManifest {
    #[serde(default)]
    pub(crate) shared_assets: Vec<SharedAssetDeclaration>,
}

/// A shared asset entry, accepted as `"path"` or `{"path": "path"}`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum SharedAssetDeclaration {
    Path(String),
    Object { path: String },
}

impl SharedAssetDeclaration {
    pub(crate) fn path(self) -> String {
        match self {
            Self::Path(path) | Self::Object { path } => path,
        }
    }
}
