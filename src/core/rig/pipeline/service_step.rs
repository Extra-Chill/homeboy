//! Service lifecycle pipeline step (start / stop / health).

use super::super::check;
use super::super::service;
use super::super::spec::{RigSpec, ServiceOp};
use crate::core::error::{Error, Result};

pub(super) fn run_service_step(rig: &RigSpec, service_id: &str, op: ServiceOp) -> Result<()> {
    match op {
        ServiceOp::Start => {
            service::start(rig, service_id)?;
            Ok(())
        }
        ServiceOp::Stop => service::stop(rig, service_id),
        ServiceOp::Health => {
            let spec = rig.services.get(service_id).ok_or_else(|| {
                Error::rig_service_failed(&rig.id, service_id, "service not declared in rig spec")
            })?;
            if let Some(health) = &spec.health {
                check::evaluate(rig, health)?;
            }
            match service::status(&rig.id, service_id)? {
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
