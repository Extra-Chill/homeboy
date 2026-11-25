//! Runner job-preparation provider hook.
//!
//! When `api_jobs` prepares a remote-runner job it needs two runner-domain
//! computations: the secret-env plan (which folds in runner/lab-declared secret
//! env names) and workload-dispatch validation. Runner is an optional Lab-
//! offload feature, so core defines the [`RunnerJobPreparationProvider`]
//! contract here and the runner layer registers an implementation at startup.
//!
//! When no provider is registered, the [`NoopRunnerJobPreparationProvider`]
//! builds a secret-env plan from the explicitly-declared names only (skipping
//! the runner/lab-specific augmentation) and performs no workload validation —
//! the correct behavior when there is no runner to dispatch to.

use std::collections::HashMap;

use crate::lab_contract::LabRunnerWorkload;
use crate::secret_env_plan::SecretEnvPlan;
use homeboy_runner_contract::RunnerCapabilityPreflight;

use crate::error::Result;

/// The runner job-preparation contract `api_jobs` depends on. Implemented by the
/// runner layer and registered at startup; core prepares remote-runner jobs
/// without depending on runner behavior.
pub trait RunnerJobPreparationProvider: Send + Sync {
    /// Compute the secret-env plan for a runner exec, folding in any
    /// runner/lab-declared secret env names.
    fn runner_exec_secret_env_plan(
        &self,
        command: &[String],
        preflight: Option<&RunnerCapabilityPreflight>,
        explicit_names: &[String],
        env: &HashMap<String, String>,
        base_plan: Option<SecretEnvPlan>,
    ) -> SecretEnvPlan;

    /// Validate that a workload may be dispatched to the given runner.
    fn validate_lab_runner_workload_dispatch(
        &self,
        workload: Option<&LabRunnerWorkload>,
        runner_id: &str,
        cwd: Option<&str>,
        command: &[String],
        secret_env_plan: &SecretEnvPlan,
        capture_patch: bool,
    ) -> Result<()>;
}

/// Default provider used when no runner layer is registered.
struct NoopRunnerJobPreparationProvider;

impl RunnerJobPreparationProvider for NoopRunnerJobPreparationProvider {
    fn runner_exec_secret_env_plan(
        &self,
        _command: &[String],
        preflight: Option<&RunnerCapabilityPreflight>,
        explicit_names: &[String],
        _env: &HashMap<String, String>,
        base_plan: Option<SecretEnvPlan>,
    ) -> SecretEnvPlan {
        if let Some(plan) = base_plan {
            return plan;
        }
        let mut names: Vec<String> = explicit_names.to_vec();
        if let Some(preflight) = preflight {
            names.extend(preflight.required_env.iter().cloned());
        }
        SecretEnvPlan::from_secret_env_names(names)
    }

    fn validate_lab_runner_workload_dispatch(
        &self,
        _workload: Option<&LabRunnerWorkload>,
        _runner_id: &str,
        _cwd: Option<&str>,
        _command: &[String],
        _secret_env_plan: &SecretEnvPlan,
        _capture_patch: bool,
    ) -> Result<()> {
        Ok(())
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RunnerJobPreparationProvider,
    noop: NoopRunnerJobPreparationProvider,
    /// Register the runner job-preparation provider. Called once at binary startup
    /// by the runner layer (via the CLI).
    register: pub fn register_runner_job_preparation_provider,
    /// Run `f` against the registered provider, or the no-op provider if none is
    /// registered.
    with: pub(crate) fn with_runner_job_preparation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_secret_env_plan_uses_base_plan_or_explicit_names() {
        let base = SecretEnvPlan::from_secret_env_names(vec!["BASE_SECRET".to_string()]);
        for (base_plan, expected) in [(None, "EXPLICIT_SECRET"), (Some(base), "BASE_SECRET")] {
            let plan = NoopRunnerJobPreparationProvider.runner_exec_secret_env_plan(
                &["cmd".to_string()],
                None,
                &["EXPLICIT_SECRET".to_string()],
                &HashMap::new(),
                base_plan,
            );
            assert_eq!(plan.secret_env_names(), vec![expected.to_string()]);
        }
    }
}
