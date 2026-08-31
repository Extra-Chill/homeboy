use crate::error::{Error, Result};
use crate::extension::catalog::list_api;
use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntry, ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest,
    EXTENSION_API_CATALOG_REQUEST_SCHEMA, EXTENSION_API_V1,
};

pub(super) struct CapabilityCatalog {
    entries: Vec<ExtensionApiCatalogEntry>,
}

impl CapabilityCatalog {
    pub(super) fn load() -> Result<Self> {
        let response = list_api(&ExtensionApiCatalogRequest {
            schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        });
        match response.failure {
            Some(failure) => Err(Error::validation_invalid_argument(
                "extension_api",
                failure.message,
                None,
                None,
            )),
            None => Ok(Self {
                entries: response.entries,
            }),
        }
    }

    pub(super) fn resolvable_entry(&self, extension_id: &str) -> Result<&ExtensionApiCatalogEntry> {
        let entry = self
            .entry(extension_id)
            .ok_or_else(|| catalog_error(extension_id, "The extension is not installed."))?;
        if entry.status == ExtensionApiCatalogEntryStatus::Invalid {
            return Err(catalog_error(
                extension_id,
                entry
                    .diagnostic
                    .as_ref()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .unwrap_or("The extension installation is invalid."),
            ));
        }
        Ok(entry)
    }

    pub(super) fn provides(&self, entry: &ExtensionApiCatalogEntry, capability_id: &str) -> bool {
        entry.descriptor.as_ref().is_some_and(|descriptor| {
            descriptor
                .capabilities
                .iter()
                .any(|provided| provided.id == capability_id)
        })
    }

    /// Invalid or missing entries remain failures only when no intact linked
    /// extension provides the requested capability.
    pub(super) fn candidates<'a>(
        &self,
        extension_ids: impl Iterator<Item = &'a String>,
        capability_id: &str,
    ) -> (Vec<String>, Vec<(String, String)>) {
        let mut matching = Vec::new();
        let mut failures = Vec::new();

        for extension_id in extension_ids {
            match self.entry(extension_id) {
                Some(entry) if self.provides(entry, capability_id) => {
                    matching.push(extension_id.clone());
                }
                Some(entry) if entry.status == ExtensionApiCatalogEntryStatus::Invalid => failures
                    .push((
                        extension_id.clone(),
                        entry
                            .diagnostic
                            .as_ref()
                            .map(|diagnostic| diagnostic.message.clone())
                            .unwrap_or_else(|| {
                                "The extension installation is invalid.".to_string()
                            }),
                    )),
                Some(_) => {}
                None => failures.push((
                    extension_id.clone(),
                    "The extension is not installed.".to_string(),
                )),
            }
        }

        matching.sort();
        failures.sort_by(|left, right| left.0.cmp(&right.0));
        (matching, failures)
    }

    pub(super) fn providers(&self, capability_id: &str) -> Vec<String> {
        let mut providers = self
            .entries
            .iter()
            .filter(|entry| entry.status == ExtensionApiCatalogEntryStatus::Available)
            .filter(|entry| self.provides(entry, capability_id))
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        providers.sort();
        providers
    }

    fn entry(&self, extension_id: &str) -> Option<&ExtensionApiCatalogEntry> {
        self.entries.iter().find(|entry| entry.id == extension_id)
    }
}

fn catalog_error(extension_id: &str, detail: &str) -> Error {
    Error::validation_invalid_argument(
        "extension",
        format!("Extension '{extension_id}' could not be read from the v1 catalog"),
        Some(detail.to_string()),
        None,
    )
}
