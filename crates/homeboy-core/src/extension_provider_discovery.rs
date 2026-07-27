//! Extension agent-runtime provider-discovery validation hook.
//!
//! When an extension is installed or repaired, core verifies that every
//! agent-runtime executor provider the extension declares is actually
//! discoverable. That check reads the extension's agent-task executor provider
//! catalog, which is agent-task behavior, so it is inverted behind this
//! provider: core owns extension install/repair, the agent-task layer validates
//! provider discovery.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! validates nothing (an install without the agent-task subsystem has no
//! agent-task providers to discover).

use crate::Result;

/// Validates that an installed extension's declared agent-runtime providers are
/// discoverable.
pub trait ExtensionProviderDiscoveryValidator: Send + Sync {
    /// Validate that the given extension's declared agent-runtime executor
    /// providers are discoverable. Returns an error describing the first
    /// missing/duplicate provider.
    fn validate_installed_extension_provider_discovery(&self, extension_id: &str) -> Result<()>;
}

struct NoopProvider;

impl ExtensionProviderDiscoveryValidator for NoopProvider {
    fn validate_installed_extension_provider_discovery(&self, _extension_id: &str) -> Result<()> {
        Ok(())
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ExtensionProviderDiscoveryValidator,
    noop: NoopProvider,
    /// Register the extension provider-discovery validator. Called once at startup
    /// by the agent-task layer.
    register: pub fn register_extension_provider_discovery_validator,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// Validate an installed extension's agent-runtime provider discovery via the
/// registered validator (or the no-op when the agent-task subsystem is absent).
pub fn validate_installed_extension_provider_discovery(extension_id: &str) -> Result<()> {
    with_provider(|p| p.validate_installed_extension_provider_discovery(extension_id))
}
