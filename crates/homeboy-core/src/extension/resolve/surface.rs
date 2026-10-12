use super::CapabilityCatalog;

/// Ownership surface for `remote_path` auto-resolution.
pub const REMOTE_PATH_SURFACE: &str = "remote_path";
/// Ownership surface for `deploy.since_tag` placeholder rewriting.
pub const SINCE_TAG_SURFACE: &str = "since_tag";
/// Ownership surface for `provides.file_extensions` dispatch.
pub const FILE_EXTENSIONS_SURFACE: &str = "provides.file_extensions";

/// Context-free deterministic fallback for consumers without a component identity.
pub fn find_installed_capability_provider(capability_id: &str) -> Option<String> {
    CapabilityCatalog::load()
        .ok()?
        .providers(capability_id)
        .into_iter()
        .next()
}
