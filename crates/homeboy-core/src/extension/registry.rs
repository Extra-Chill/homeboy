//! Validation an extension install, replace, or relink runs after setup.
//!
//! The check has two halves, and only one of them belongs to core.
//!
//! - **Is every declared executor registrable?** A malformed
//!   `agent_runtimes[].agent_task_executors[]` entry, a duplicate executor id,
//!   or an executor the extension never advertises is wrong whether or not the
//!   agent-task subsystem exists. Core answers this from the typed Extension API
//!   registration inventory.
//! - **Does each registered executor resolve to something dispatchable?** That
//!   requires the agent-task layer's resolved provider type, which core cannot
//!   see, so the caller supplies it.
//!
//! The second half used to be an ambient global that the CLI registered at
//! startup. Nothing in a signature said the check existed, and with no provider
//! registered the no-op silently validated nothing — which is what regressed
//! #12206. It is now an explicit parameter: a caller that has the agent-task
//! subsystem passes it, and a caller that does not cannot accidentally appear to
//! have run a check it never ran.

use homeboy_extension_contract::api::v1::{
    ExtensionApiAgentTaskExecutorInventoryRequest,
    EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA, EXTENSION_API_V1,
};

use crate::extension::agent_task_executor_api::AgentTaskExecutorApi;
use crate::{Error, Result};

/// Resolves registered agent-task executors into dispatchable providers.
///
/// Implemented by the agent-task layer, which owns the resolved provider type.
pub trait ExtensionExecutorDiscovery {
    /// Confirm every executor the extension registers resolves to a provider
    /// this host can dispatch. Returns an error describing the first executor
    /// that does not.
    fn validate_registered_executors(&self, extension_id: &str) -> Result<()>;
}

/// The validation an extension lifecycle mutation runs after setup.
///
/// Construct this at the composition root, where both core and the agent-task
/// subsystem are visible, and pass it into install, replace, and relink.
#[derive(Default, Clone, Copy)]
pub struct ExtensionLifecycleValidation<'a> {
    executor_discovery: Option<&'a dyn ExtensionExecutorDiscovery>,
}

impl<'a> ExtensionLifecycleValidation<'a> {
    /// Validate declarations only.
    ///
    /// This is the correct choice for a host without the agent-task subsystem:
    /// there are no executors to resolve, so there is no resolution to check.
    /// It is not a way to skip validation — declaration registrability is still
    /// enforced.
    pub fn declaration_only() -> Self {
        Self {
            executor_discovery: None,
        }
    }

    pub fn with_executor_discovery(discovery: &'a dyn ExtensionExecutorDiscovery) -> Self {
        Self {
            executor_discovery: Some(discovery),
        }
    }

    /// Run both halves in order. Declarations are always checked; resolution is
    /// checked when the caller supplied a discoverer.
    pub fn validate_installed_extension(&self, extension_id: &str) -> Result<()> {
        validate_registered_executor_declarations(extension_id)?;
        match self.executor_discovery {
            Some(discovery) => discovery.validate_registered_executors(extension_id),
            None => Ok(()),
        }
    }
}

/// Reject an extension whose declared executors cannot be registered.
///
/// The typed inventory is the single place declarations become identities, so an
/// entry it marks unusable is rejected here rather than installed and then found
/// undispatchable later.
fn validate_registered_executor_declarations(extension_id: &str) -> Result<()> {
    let inventory =
        AgentTaskExecutorApi::discover(&ExtensionApiAgentTaskExecutorInventoryRequest {
            schema: EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        });

    for executor in inventory.registered_by(extension_id) {
        let Some(diagnostic) = executor.diagnostic.as_ref() else {
            continue;
        };
        return Err(Error::validation_invalid_argument(
            "agent_runtimes.agent_task_executors",
            format!(
                "Extension '{}' declares an agent-task executor that cannot be registered: {}",
                extension_id, diagnostic.message
            ),
            Some(if executor.id.is_empty() {
                executor.runtime_id.clone()
            } else {
                executor.id.clone()
            }),
            None,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct RecordingDiscovery {
        called: Cell<bool>,
        result: Option<&'static str>,
    }

    impl ExtensionExecutorDiscovery for RecordingDiscovery {
        fn validate_registered_executors(&self, _extension_id: &str) -> Result<()> {
            self.called.set(true);
            match self.result {
                Some(message) => Err(Error::validation_invalid_argument(
                    "source", message, None, None,
                )),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn declaration_only_validation_never_reports_a_resolution_it_did_not_run() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // No agent-task subsystem: there is nothing to resolve, so this must
            // succeed rather than silently claim a check that never ran.
            ExtensionLifecycleValidation::declaration_only()
                .validate_installed_extension("absent-extension")
                .expect("declaration-only validation accepts an extension with no executors");
        });
    }

    #[test]
    fn supplied_discovery_runs_and_its_rejection_fails_the_lifecycle_mutation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let discovery = RecordingDiscovery {
                called: Cell::new(false),
                result: Some("declared provider was not discoverable"),
            };

            let error = ExtensionLifecycleValidation::with_executor_discovery(&discovery)
                .validate_installed_extension("absent-extension")
                .expect_err("a supplied discoverer's rejection must fail the mutation");

            assert!(discovery.called.get(), "the supplied discoverer must run");
            assert!(
                error.message.contains("not discoverable"),
                "got {}",
                error.message
            );
        });
    }

    #[test]
    fn supplied_discovery_acceptance_passes_the_lifecycle_mutation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let discovery = RecordingDiscovery {
                called: Cell::new(false),
                result: None,
            };

            ExtensionLifecycleValidation::with_executor_discovery(&discovery)
                .validate_installed_extension("absent-extension")
                .expect("an accepted extension installs");

            assert!(discovery.called.get(), "the supplied discoverer must run");
        });
    }
}
