//! Controller-upgrade admission hook.

use homeboy_core::error::Error;
use homeboy_core::error::Result;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ControllerUpgradeAdmission {
    pub schema: &'static str,
    pub active: usize,
    pub stale: usize,
    pub suspect: usize,
    pub unreconciled: usize,
    pub reconcilable: usize,
    pub record_health: serde_json::Value,
    pub blockers: Vec<ControllerUpgradeBlocker>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ControllerUpgradeBlocker {
    pub run_id: String,
    pub liveness: &'static str,
    pub reason: String,
    pub recovery_command: String,
}

impl ControllerUpgradeAdmission {
    pub fn allows_controller_replacement(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Supplies agent-task liveness evidence before the controller binary changes.
pub trait ControllerUpgradeAdmissionProvider: Send + Sync {
    fn controller_upgrade_admission(&self) -> Result<ControllerUpgradeAdmission>;
}

struct NoopControllerUpgradeAdmissionProvider;

impl ControllerUpgradeAdmissionProvider for NoopControllerUpgradeAdmissionProvider {
    fn controller_upgrade_admission(&self) -> Result<ControllerUpgradeAdmission> {
        Ok(ControllerUpgradeAdmission {
            schema: "homeboy/controller-upgrade-admission/v1",
            active: 0,
            stale: 0,
            suspect: 0,
            unreconciled: 0,
            reconcilable: 0,
            record_health: serde_json::Value::Null,
            blockers: Vec::new(),
        })
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ControllerUpgradeAdmissionProvider,
    noop: NoopControllerUpgradeAdmissionProvider,
    register: pub fn register_controller_upgrade_admission_provider,
    with: pub(crate) fn with_controller_upgrade_admission,
}

/// Run the admission provider from a candidate controller before an installer
/// promotes it over the controller that invoked the upgrade.
pub fn controller_upgrade_admission() -> Result<ControllerUpgradeAdmission> {
    with_controller_upgrade_admission(|provider| provider.controller_upgrade_admission())
}

pub fn ensure_controller_upgrade_admission() -> Result<ControllerUpgradeAdmission> {
    let admission = controller_upgrade_admission()?;
    if admission.allows_controller_replacement() {
        return Ok(admission);
    }

    let commands = admission
        .blockers
        .iter()
        .map(|blocker| blocker.recovery_command.clone())
        .collect::<Vec<_>>();
    let mut error = Error::validation_invalid_argument(
        "controller_upgrade",
        format!(
            "refusing to replace the controller binary while durable orchestration ownership is live or unverified: {}",
            admission
                .blockers
                .iter()
                .map(|blocker| blocker.run_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None,
        Some(commands),
    );
    error.details["controller_upgrade_admission"] = serde_json::to_value(&admission)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Err(error)
}
