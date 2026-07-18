use super::{load_extension, write_source_metadata};
use homeboy_core::error::{Error, Result};
use homeboy_core::extension_store::is_extension_linked;
use homeboy_core::extension_update_check::{read_source_revision, read_source_url};
use homeboy_core::git;
use homeboy_core::paths;

pub use homeboy_extension_contract::source_metadata_repair::SourceMetadataRepair;

#[derive(Debug)]
pub struct SourceMetadataResolution {
    pub url: String,
    pub(crate) repair: Option<SourceMetadataRepair>,
}

/// Resolve extension source provenance without repairing metadata on disk.
///
/// This is suitable for reporting and remote materialization paths where merely
/// inspecting installed extensions must not modify local state.
pub fn resolve_source_url_read_only(extension_id: &str) -> Result<String> {
    let extension = load_extension(extension_id)?;
    let extension_dir = paths::extension(extension_id)?;

    manifest_source_url(&extension)
        .or_else(|| read_source_url(&extension_dir).and_then(normalize_source_url))
        .or_else(|| {
            is_extension_linked(extension_id)
                .then(|| git::remote_origin_url(&extension_dir))
                .flatten()
                .and_then(|url| normalize_source_url(url))
        })
        .ok_or_else(|| missing_source_url_error(extension_id, extension.extension_path.as_deref()))
}

pub fn resolve_source_url(extension_id: &str) -> Result<SourceMetadataResolution> {
    let extension = load_extension(extension_id)?;
    let extension_dir = paths::extension(extension_id)?;
    let metadata_url = homeboy_core::extension_update_check::read_source_url(&extension_dir);

    let manifest_source_url = manifest_source_url(&extension);

    if let Some(source_url) = manifest_source_url {
        let repair = if metadata_url.as_deref() != Some(source_url.as_str()) {
            write_source_metadata(
                &extension_dir,
                &source_url,
                homeboy_core::extension_update_check::read_source_revision(extension_id),
            );
            Some(SourceMetadataRepair {
                source_url: source_url.clone(),
                reason: "restored .source-url from manifest sourceUrl".to_string(),
                repair_command: repair_command(extension_id, &source_url),
            })
        } else {
            None
        };

        return Ok(SourceMetadataResolution {
            url: source_url,
            repair,
        });
    }

    if let Some(source_url) = metadata_url {
        return Ok(SourceMetadataResolution {
            url: source_url,
            repair: None,
        });
    }

    Err(missing_source_url_error(
        extension_id,
        extension.extension_path.as_deref(),
    ))
}

fn manifest_source_url(extension: &crate::ExtensionManifest) -> Option<String> {
    extension
        .source_url
        .clone()
        .and_then(normalize_source_url)
        .or_else(|| {
            extension
                .extra
                .get("sourceUrl")
                .and_then(|value| value.as_str())
                .and_then(normalize_source_url)
        })
}

fn normalize_source_url(value: impl AsRef<str>) -> Option<String> {
    let value = value.as_ref().trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn missing_source_url_error(extension_id: &str, extension_path: Option<&str>) -> Error {
    let mut err = Error::validation_invalid_argument(
        "extension_id",
        format!(
            "Extension '{}' has no sourceUrl or .source-url metadata, so Homeboy cannot determine where to update it from.",
            extension_id
        ),
        Some(extension_id.to_string()),
        None,
    )
    .with_hint(format!(
        "Repair by reinstalling from the original source: homeboy extension install <url> --id {}",
        extension_id
    ));

    if let Some(path) = extension_path {
        err = err.with_hint(format!("Installed extension path: {}", path));
    }

    err
}

fn repair_command(extension_id: &str, source_url: &str) -> String {
    format!(
        "homeboy extension install {} --id {}",
        source_url, extension_id
    )
}
