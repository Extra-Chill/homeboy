//! Source-metadata repair contract type.
//!
//! Pure data describing a repair that reconciled an extension's on-disk source
//! metadata with its manifest. The behavior that produces it (resolving source
//! URLs, rewriting metadata files) lives in `homeboy_extension`; only the
//! serializable result shape lives here so it can travel through update/report
//! contract types without dragging that behavior into the leaf crate.

#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceMetadataRepair {
    pub source_url: String,
    pub reason: String,
    pub repair_command: String,
}
