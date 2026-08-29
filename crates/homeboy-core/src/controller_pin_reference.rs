//! Controller-runtime pin-reference hook.
//!
//! Controller runtime pins (immutable homeboy executables) are retained while a
//! durable agent-task record can still mutate them and remains inside the
//! configured retention window. In-flight (`Queued` / `Running`) records keep
//! their pin regardless of age so an executing run cannot lose its binary.
//! Outside that window the pin is reclaimable; `recover_pin` can republish it
//! from a verified artifact or the recorded source revision.
//!
//! Discovering which pins are still referenced requires reading the agent-task
//! lifecycle store and inspecting each record's lifecycle-action eligibility
//! and age, which is agent-task behavior. It is inverted behind this provider
//! so the controller runtime's retention logic (pin classification, disk scan,
//! pruning) stays in core without core depending on the agent-task subsystem.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! provider reports zero referenced pins. That is safe for the read-only
//! retention report, and pruning is always an explicit caller opt-in.

use std::path::PathBuf;

use crate::Result;

/// Why a durable record still protects a controller-runtime pin.
///
/// Discriminant order is weakest-to-strongest so [`Ord`] picks the constraint
/// that must win when several records name the same path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ControllerPinProtectionReason {
    /// A mutating lifecycle action remains available or indeterminate and the
    /// record is still inside the configured retention window.
    ProtectedByPendingMutation,
    /// The record is genuinely in flight (`Queued` or `Running`).
    ProtectedInFlight,
}

impl ControllerPinProtectionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProtectedByPendingMutation => "protected_by_pending_mutation",
            Self::ProtectedInFlight => "protected_in_flight",
        }
    }
}

/// One originating or pinned executable still protected by a durable record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferencedControllerPin {
    pub path: PathBuf,
    pub reason: ControllerPinProtectionReason,
}

/// Supplies the controller-runtime executable paths still referenced by
/// durable agent-task records that retain their pin.
pub trait ControllerPinReferenceProvider: Send + Sync {
    /// Return every originating and pinned executable path referenced by an
    /// agent-task record whose lifecycle still retains its runtime. Returned
    /// paths are raw record references; callers select the paths they own or
    /// need to protect.
    fn referenced_controller_pins(&self) -> Result<Vec<ReferencedControllerPin>>;
}

struct NoopProvider;

impl ControllerPinReferenceProvider for NoopProvider {
    fn referenced_controller_pins(&self) -> Result<Vec<ReferencedControllerPin>> {
        Ok(Vec::new())
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ControllerPinReferenceProvider,
    noop: NoopProvider,
    /// Register the controller pin-reference provider. Called once at startup by the
    /// agent-task layer.
    register: pub fn register_controller_pin_reference_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// The controller-runtime executables still referenced by agent-task records
/// that retain their pin, via the registered provider (or an empty set when
/// the agent-task subsystem is absent).
pub(crate) fn referenced_controller_pins() -> Result<Vec<ReferencedControllerPin>> {
    with_provider(|p| p.referenced_controller_pins())
}
