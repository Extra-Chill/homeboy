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
use std::sync::Mutex;

use crate::lab_contract::RunnerWorkload;
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
    fn validate_runner_workload_dispatch(
        &self,
        workload: Option<&RunnerWorkload>,
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

    fn validate_runner_workload_dispatch(
        &self,
        _workload: Option<&RunnerWorkload>,
        _runner_id: &str,
        _cwd: Option<&str>,
        _command: &[String],
        _secret_env_plan: &SecretEnvPlan,
        _capture_patch: bool,
    ) -> Result<()> {
        Ok(())
    }
}

static PROVIDER: Mutex<Option<Box<dyn RunnerJobPreparationProvider>>> = Mutex::new(None);

/// Register the runner job-preparation provider. Called once at binary startup
/// by the runner layer (via the CLI).
pub fn register_runner_job_preparation_provider(provider: Box<dyn RunnerJobPreparationProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Run `f` against the registered provider, or the no-op provider if none is
/// registered.
pub(crate) fn with_runner_job_preparation<T>(
    f: impl FnOnce(&dyn RunnerJobPreparationProvider) -> T,
) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopRunnerJobPreparationProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_secret_env_plan_uses_explicit_names() {
        let noop = NoopRunnerJobPreparationProvider;
        let plan = noop.runner_exec_secret_env_plan(
            &["cmd".to_string()],
            None,
            &["EXPLICIT_SECRET".to_string()],
            &HashMap::new(),
            None,
        );
        assert!(plan
            .secret_env_names()
            .contains(&"EXPLICIT_SECRET".to_string()));
    }

    #[test]
    fn noop_secret_env_plan_prefers_base_plan() {
        let noop = NoopRunnerJobPreparationProvider;
        let base = SecretEnvPlan::from_secret_env_names(vec!["BASE_SECRET".to_string()]);
        let plan = noop.runner_exec_secret_env_plan(
            &["cmd".to_string()],
            None,
            &["EXPLICIT_SECRET".to_string()],
            &HashMap::new(),
            Some(base),
        );
        let names = plan.secret_env_names();
        assert!(names.contains(&"BASE_SECRET".to_string()));
    }

    #[test]
    fn noop_workload_validation_passes() {
        let noop = NoopRunnerJobPreparationProvider;
        let plan = SecretEnvPlan::from_secret_env_names(Vec::<String>::new());
        assert!(noop
            .validate_runner_workload_dispatch(
                None,
                "runner",
                None,
                &["cmd".to_string()],
                &plan,
                false
            )
            .is_ok());
    }
}
