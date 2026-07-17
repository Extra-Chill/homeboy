//! Controller-runtime pin-reference hook.
//!
//! Controller runtime pins (immutable homeboy executables) are retained while a
//! durable agent-task record can still operate on them — a queued, running, or
//! recoverable-partial record keeps its originating pinned executable alive so
//! lifecycle recovery does not lose the binary it must re-run.
//!
//! Discovering which pins are still referenced requires reading the agent-task
//! lifecycle store and inspecting each record's state and metadata, which is
//! agent-task behavior. It is inverted behind this provider so the controller
//! runtime's retention logic (pin classification, disk scan, pruning) stays in
//! core without core depending on the agent-task subsystem.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! provider reports zero referenced pins. That is safe for the read-only
//! retention report, and pruning is always an explicit caller opt-in.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::Result;

/// Supplies the controller-runtime pin paths still referenced by nonterminal
/// durable agent-task records.
pub trait ControllerPinReferenceProvider: Send + Sync {
    /// Return every pinned-executable path referenced by an agent-task record
    /// whose state still retains its pin (queued, running, or recoverable).
    /// Returned paths are raw record references; the caller filters them to the
    /// content-addressed pins it owns.
    fn referenced_controller_pins(&self) -> Result<Vec<PathBuf>>;
}

struct NoopProvider;

impl ControllerPinReferenceProvider for NoopProvider {
    fn referenced_controller_pins(&self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn ControllerPinReferenceProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn ControllerPinReferenceProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the controller pin-reference provider. Called once at startup by the
/// agent-task layer.
pub fn register_controller_pin_reference_provider(
    provider: Box<dyn ControllerPinReferenceProvider>,
) {
    let mut slot = provider_slot()
        .lock()
        .expect("controller pin reference provider lock");
    *slot = Some(provider);
}

/// The controller-runtime pins still referenced by nonterminal agent-task
/// records, via the registered provider (or an empty set when the agent-task
/// subsystem is absent).
pub(crate) fn referenced_controller_pins() -> Result<Vec<PathBuf>> {
    let slot = provider_slot()
        .lock()
        .expect("controller pin reference provider lock");
    match slot.as_deref() {
        Some(provider) => provider.referenced_controller_pins(),
        None => NoopProvider.referenced_controller_pins(),
    }
}
