//! Structured-sidecar declaration resolution.
//!
//! The pure contract types live in `homeboy-extension-contract`; this module
//! This module provides the resolution logic that depends on core's
//! run-dir file constants and `structured_sidecar` defaults.

use homeboy_core::structured_sidecar;

use homeboy_extension_contract::sidecar_config::{
    StructuredSidecarContract, StructuredSidecarDeclaration,
};

/// Resolve a structured-sidecar contract into a concrete declaration for the
/// given sidecar name, applying core's default paths and producers.
pub(super) fn structured_sidecar_declaration(
    contract: &StructuredSidecarContract,
    name: &str,
) -> Option<StructuredSidecarDeclaration> {
    match contract {
        StructuredSidecarContract::Enabled(true) => Some(StructuredSidecarDeclaration {
            name: name.to_string(),
            path: default_structured_sidecar_path(name),
            schema_version: structured_sidecar::default_schema_version(name).map(str::to_string),
            producer: default_structured_sidecar_producer(name),
        }),
        StructuredSidecarContract::Enabled(false) => None,
        StructuredSidecarContract::Detail(detail) => {
            if !detail.enabled {
                return None;
            }

            Some(StructuredSidecarDeclaration {
                name: name.to_string(),
                path: detail
                    .path
                    .clone()
                    .unwrap_or_else(|| default_structured_sidecar_path(name)),
                schema_version: detail.schema_version.clone(),
                producer: detail
                    .producer
                    .clone()
                    .or_else(|| default_structured_sidecar_producer(name)),
            })
        }
    }
}

/// Default run-dir-relative path for a sidecar name.
///
/// `structured_sidecar::REGISTRY` is the *only* source of defaults. Until
/// #11121 a second ~15-key table lived here as a fallback, reached only when
/// the registry missed. Because both lookups early-returned from the registry,
/// nine of its fifteen arms were unreachable, and the five keys it alone knew
/// (`lint.producers`, `test.coverage`, `resource.summary`, `producer.summary`,
/// `findings`) got a path from here but no schema version and no payload
/// validation — the registry is what carries those. Those five moved into the
/// registry and the table is gone.
///
/// An unknown name still falls back to itself, so an extension may declare a
/// sidecar core has no contract for; it simply gets no schema version and no
/// validation, which is the honest reading of "core does not know this key".
fn default_structured_sidecar_path(name: &str) -> String {
    structured_sidecar::default_path(name)
        .unwrap_or(name)
        .to_string()
}

fn default_structured_sidecar_producer(name: &str) -> Option<String> {
    structured_sidecar::default_producer(name).map(str::to_string)
}
