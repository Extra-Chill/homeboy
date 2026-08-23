//! Service lifecycle pipeline step (start / stop / health).

use super::super::check;
use super::super::service;
use super::super::spec::{RigSpec, ServiceOp};
use super::super::state::RigStateStore;
use homeboy_core::error::{Error, Result};

pub(super) fn run_service_step(
    state_store: &RigStateStore,
    rig: &RigSpec,
    service_id: &str,
    op: ServiceOp,
) -> Result<()> {
    match op {
        ServiceOp::Start => {
            service::start(state_store, rig, service_id)?;
            Ok(())
        }
        ServiceOp::Stop => service::stop(state_store, rig, service_id),
        ServiceOp::Health => {
            let spec = rig.services.get(service_id).ok_or_else(|| {
                Error::rig_service_failed(&rig.id, service_id, "service not declared in rig spec")
            })?;
            if let Some(health) = &spec.health {
                check::evaluate(rig, health)?;
            }
            match service::status(state_store, &rig.id, service_id)? {
                service::ServiceStatus::Running(_) => Ok(()),
                service::ServiceStatus::Stopped => Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    "service is stopped",
                )),
                service::ServiceStatus::Stale(pid) => Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    format!("recorded PID {} is not alive", pid),
                )),
            }
        }
    }
}
