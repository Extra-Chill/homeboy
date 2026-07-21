//! Lab-offload contract types and execution hook.
//!
//! `lab_routing` (core) routes a command to Lab offload and consumes the
//! outcome, but the actual offload execution (workspace materialization, remote
//! dispatch, patch capture) is runner behavior. The request/command/outcome
//! types are core-plan-based, so they live here; the execution itself is
//! inverted behind [`LabOffloadProvider`] so `lab_routing` stays in core while
//! the runner crate performs the offload.
//!
//! With no provider registered the no-op provider errors clearly — offload is
//! only reached when a Lab command was actually dispatched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::lab_contract::{
    LabCommandContract, LabRigWorkloadArguments, LabRunnerWorkloadCapability, LabSourcePathMode,
    LabWorkspaceModePolicy,
};
use crate::plan::HomeboyPlan;

/// Per-job overrides carried into a Lab offload.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabJobOverrides {
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub workspace_root: Option<String>,
}

/// A resolved Lab command with its required extensions/capabilities/workload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LabOffloadCommand {
    pub command: LabCommandContract,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<LabRunnerWorkloadCapability>,
    pub workload: Option<LabRigWorkloadArguments>,
}

impl std::ops::Deref for LabOffloadCommand {
    type Target = LabCommandContract;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

impl std::ops::DerefMut for LabOffloadCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}

pub type LabOffloadSourcePathMode = LabSourcePathMode;
pub type LabOffloadWorkspaceModePolicy = LabWorkspaceModePolicy;

/// The outcome of a Lab offload: a controller plan plus (for executed offloads)
/// captured output.
pub enum LabOffloadOutcome {
    RunLocal {
        plan: HomeboyPlan,
        metadata: Option<serde_json::Value>,
        messages: Vec<String>,
    },
    Offloaded {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
        output_file_content: Option<String>,
    },
    InFlight {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
        output_file_content: Option<String>,
    },
}

/// Executes a Lab offload for a routed request. Implemented by the runner layer.
pub trait LabOffloadProvider: Send + Sync {
    fn execute_lab_offload(
        &self,
        request: crate::lab_routing::LabRoutingRequest<'_>,
    ) -> Result<LabOffloadOutcome>;
}

struct NoopProvider;

impl LabOffloadProvider for NoopProvider {
    fn execute_lab_offload(
        &self,
        _request: crate::lab_routing::LabRoutingRequest<'_>,
    ) -> Result<LabOffloadOutcome> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot execute a Lab offload",
        ))
    }
}

fn provider_slot() -> &'static Mutex<Option<Arc<dyn LabOffloadProvider>>> {
    static PROVIDER: Mutex<Option<Arc<dyn LabOffloadProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the Lab-offload provider. Called once at startup by the runner layer.
pub fn register_lab_offload_provider(provider: Arc<dyn LabOffloadProvider>) {
    let mut slot = provider_slot().lock().expect("lab offload provider lock");
    *slot = Some(provider);
}

/// Execute a Lab offload via the registered provider (or the no-op provider).
/// The provider `Arc` is cloned out before executing so the registry lock is
/// not held during the (potentially long) offload.
pub(crate) fn execute_lab_offload(
    request: crate::lab_routing::LabRoutingRequest<'_>,
) -> Result<LabOffloadOutcome> {
    let provider = {
        let slot = provider_slot().lock().expect("lab offload provider lock");
        slot.as_ref().map(Arc::clone)
    };
    match provider {
        Some(provider) => provider.execute_lab_offload(request),
        None => NoopProvider.execute_lab_offload(request),
    }
}
