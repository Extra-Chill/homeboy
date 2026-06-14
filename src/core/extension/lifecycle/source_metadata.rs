use super::{load_extension, read_source_revision, write_source_metadata};
use crate::core::error::{Error, Result};
use crate::core::paths;

#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceMetadataRepair {
    pub source_url: String,
    pub reason: String,
    pub repair_command: String,
}

#[derive(Debug)]
pub(crate) struct SourceMetadataResolution {
    pub(crate) url: String,
    pub(crate) repair: Option<SourceMetadataRepair>,
}

pub(crate) fn resolve_source_url(extension_id: &str) -> Result<SourceMetadataResolution> {
    let extension = load_extension(extension_id)?;
    let extension_dir = paths::extension(extension_id)?;
    let metadata_path = extension_dir.join(".source-url");
    let metadata_url = std::fs::read_to_string(&metadata_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let manifest_source_url = extension.source_url.clone().or_else(|| {
        extension
            .extra
            .get("sourceUrl")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
    });

    if let Some(source_url) = manifest_source_url {
        let repair = if metadata_url.as_deref() != Some(source_url.as_str()) {
            write_source_metadata(
                &extension_dir,
                &source_url,
                read_source_revision(extension_id),
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

    if let Some(path) = extension.extension_path {
        err = err.with_hint(format!("Installed extension path: {}", path));
    }

    Err(err)
}

fn repair_command(extension_id: &str, source_url: &str) -> String {
    format!(
        "homeboy extension install {} --id {}",
        source_url, extension_id
    )
}
