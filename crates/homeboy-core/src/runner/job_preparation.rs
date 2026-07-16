//! Runner-side implementation of core's `RunnerJobPreparationProvider` hook.
//!
//! Core's `api_jobs` calls this contract when preparing remote-runner jobs, so
//! it can compute the secret-env plan and validate workload dispatch without
//! depending on runner behavior directly.

use std::collections::HashMap;

use crate::lab_contract::RunnerWorkload;
use crate::secret_env_plan::SecretEnvPlan;
use homeboy_runner_contract::RunnerCapabilityPreflight;

use crate::api_jobs::RunnerJobPreparationProvider;
use crate::error::Result;

/// The runner layer's `RunnerJobPreparationProvider`. Registered with core at
/// startup.
pub struct RunnerJobPreparation;

impl RunnerJobPreparationProvider for RunnerJobPreparation {
    fn runner_exec_secret_env_plan(
        &self,
        command: &[String],
        preflight: Option<&RunnerCapabilityPreflight>,
        explicit_names: &[String],
        env: &HashMap<String, String>,
        base_plan: Option<SecretEnvPlan>,
    ) -> SecretEnvPlan {
        super::execution::runner_exec_secret_env_plan(
            command,
            preflight,
            explicit_names,
            env,
            base_plan,
        )
    }

    fn validate_runner_workload_dispatch(
        &self,
        workload: Option<&RunnerWorkload>,
        runner_id: &str,
        cwd: Option<&str>,
        command: &[String],
        secret_env_plan: &SecretEnvPlan,
        capture_patch: bool,
    ) -> Result<()> {
        super::workload::validate_runner_workload_dispatch(
            workload,
            runner_id,
            cwd,
            command,
            secret_env_plan,
            capture_patch,
        )
    }
}

/// Register the runner job-preparation provider with core. Called once at
/// startup.
pub fn register() {
    crate::api_jobs::register_runner_job_preparation_provider(Box::new(RunnerJobPreparation));
}
