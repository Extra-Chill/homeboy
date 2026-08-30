//! Extension agent-runtime provider-discovery validation hook.
//!
//! When an extension is installed or repaired, core verifies that every
//! agent-runtime executor provider the extension declares is actually
//! discoverable. That check reads the extension's agent-task executor provider
//! catalog, which is agent-task behavior, so it is inverted behind this
//! provider: core owns extension install/repair, the agent-task layer validates
//! provider discovery.
//!
//! The check has two halves, and only one of them is agent-task behavior:
//!
//! - **Is the declaration well-formed?** A malformed
//!   `agent_runtimes[].agent_task_executors[]` entry is malformed whether or not
//!   the agent-task subsystem is present, so this is validated unconditionally
//!   against the shared declaration contract.
//! - **Is the declared provider discoverable?** This reads the extension's
//!   agent-task executor provider catalog, which *is* agent-task behavior, so it
//!   stays inverted behind the provider below.
//!
//! Collapsing both halves into the inverted hook is what regressed #12206: with
//! no provider registered the no-op validated *nothing*, so an extension
//! declaring a provider that cannot be parsed installed silently instead of
//! being rejected and rolled back.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! still validates no *discoverability* — an install without the agent-task
//! subsystem has no agent-task providers to discover.

use homeboy_extension_contract::agent_task_executor_declaration::parse_agent_task_executor_declaration;

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

/// Validate an installed extension's agent-runtime provider discovery.
///
/// Declaration well-formedness is checked unconditionally; discoverability is
/// delegated to the registered validator (or the no-op when the agent-task
/// subsystem is absent).
pub fn validate_installed_extension_provider_discovery(extension_id: &str) -> Result<()> {
    validate_declared_agent_task_executors(extension_id)?;
    with_provider(|p| p.validate_installed_extension_provider_discovery(extension_id))
}

/// Reject an extension that declares an agent-task executor which cannot be
/// parsed, independently of whether the agent-task subsystem is registered.
///
/// A manifest that cannot be loaded at all is left to the caller's own manifest
/// handling rather than reported here as a provider-declaration fault.
fn validate_declared_agent_task_executors(extension_id: &str) -> Result<()> {
    let Ok(extension) = crate::extension::catalog::load_extension(extension_id) else {
        return Ok(());
    };

    for runtime in &extension.agent_runtimes {
        for declared in &runtime.agent_task_executors {
            parse_agent_task_executor_declaration(&extension.id, &runtime.id, declared)?;
        }
    }

    Ok(())
}
