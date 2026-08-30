//! Lab runner implementation of core's transport-neutral execution service.

use homeboy_core::error::{Error, Result};
use homeboy_core::runner::RunnerExecutionProvider;
use homeboy_runner_contract::{
    PathMaterializationPlan, RunnerExecutionEnvelope, RunnerExecutionRecord,
};

struct LabRunnerExecutionProvider;

impl RunnerExecutionProvider for LabRunnerExecutionProvider {
    fn submit(&self, request: RunnerExecutionEnvelope) -> Result<RunnerExecutionRecord> {
        let runner_id = request
            .dispatch
            .as_ref()
            .map(|dispatch| dispatch.runner_id.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "dispatch",
                    "runner execution submission requires a dispatch target",
                    Some(request.envelope_id.clone()),
                    None,
                )
            })?;
        let options = options_from_request(request)?;
        let (output, _) = crate::execution::exec(&runner_id, options)?;
        output.execution_record.ok_or_else(|| {
            Error::internal_unexpected(
                "runner execution completed without a canonical execution record",
            )
        })
    }
}

fn options_from_request(
    mut request: RunnerExecutionEnvelope,
) -> Result<crate::execution::RunnerExecOptions> {
    let dispatch = request.dispatch.take().ok_or_else(|| {
        Error::validation_invalid_argument(
            "dispatch",
            "runner execution submission requires a dispatch target",
            Some(request.envelope_id.clone()),
            None,
        )
    })?;
    let path_materialization_plan = request
        .metadata
        .get("path_materialization_plan")
        .cloned()
        .map(serde_json::from_value::<PathMaterializationPlan>)
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "metadata.path_materialization_plan",
                format!("invalid runner path materialization plan: {error}"),
                Some(request.envelope_id.clone()),
                None,
            )
        })?;
    let mut required_extensions = request
        .lab_runner_workload
        .as_ref()
        .map(|workload| workload.required_extensions.clone())
        .unwrap_or_default();
    required_extensions.extend(dispatch.extension_env_providers.iter().cloned());
    required_extensions.sort();
    required_extensions.dedup();
    let secret_env_names = request
        .secret_env
        .as_ref()
        .map(|plan| plan.secret_env_names())
        .unwrap_or_default();
    let mut env: std::collections::HashMap<String, String> = request
        .secret_env
        .as_ref()
        .map(|plan| plan.public_env.clone().into_iter().collect())
        .unwrap_or_default();
    env.extend(dispatch.env);
    let env_materialization = request.env_materialization.or_else(|| {
        request
            .secret_env
            .as_ref()
            .and_then(|plan| plan.env_materialization.clone())
    });

    Ok(crate::execution::RunnerExecOptions {
        execution_context:
            homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::local(
                "homeboy-runner-api",
            ),
        cwd: dispatch.cwd,
        project_id: dispatch.project_id,
        command: dispatch.command,
        env,
        secret_env_names,
        secret_env_plan: request.secret_env,
        env_materialization,
        capture_patch: request.mutation_policy.capture_patch,
        raw_exec: true,
        source_snapshot: dispatch.source_snapshot,
        path_materialization_plan,
        required_extensions,
        extension_env_providers: dispatch.extension_env_providers,
        require_paths: dispatch.require_paths,
        lab_runner_workload: request.lab_runner_workload,
        run_id: request.result_refs.run_id,
        print_handoff: false,
        ..crate::execution::RunnerExecOptions::default()
    })
}

/// Register the Lab runner as the process implementation of core's Runner API.
pub fn register() {
    homeboy_core::runner::register_runner_execution_provider(std::sync::Arc::new(
        LabRunnerExecutionProvider,
    ));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use homeboy_runner_contract::{
        RunnerExecutionDispatch, RunnerExecutionEnvelope, RunnerExecutionMutationPolicy,
    };
    use homeboy_core::secret_env_plan::SecretEnvPlan;

    use super::options_from_request;

    #[test]
    fn canonical_request_compiles_to_the_existing_transport_executor() {
        let mut request = RunnerExecutionEnvelope::planned("exec-1", "runner_api");
        request.dispatch = Some(RunnerExecutionDispatch {
            runner_id: "runner-a".to_string(),
            project_id: Some("project-a".to_string()),
            operation: "runner.exec".to_string(),
            command: vec!["printf".to_string(), "ok".to_string()],
            cwd: Some("/workspace".to_string()),
            env: HashMap::from([("PUBLIC".to_string(), "value".to_string())]),
            source_snapshot: None,
            require_paths: vec!["/cache".to_string()],
            extension_env_providers: vec!["runtime-a".to_string()],
        });
        request.mutation_policy = RunnerExecutionMutationPolicy {
            capture_patch: true,
            ..Default::default()
        };
        request.secret_env = Some(SecretEnvPlan {
            public_env: [("FROM_PLAN".to_string(), "projected".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        });

        let options = options_from_request(request).expect("compile request");

        assert_eq!(options.command, vec!["printf", "ok"]);
        assert_eq!(options.project_id.as_deref(), Some("project-a"));
        assert_eq!(options.cwd.as_deref(), Some("/workspace"));
        assert_eq!(options.env.get("PUBLIC").map(String::as_str), Some("value"));
        assert_eq!(
            options.env.get("FROM_PLAN").map(String::as_str),
            Some("projected")
        );
        assert_eq!(options.require_paths, vec!["/cache"]);
        assert_eq!(options.required_extensions, vec!["runtime-a"]);
        assert_eq!(options.extension_env_providers, vec!["runtime-a"]);
        assert!(options.capture_patch);
        assert!(options.raw_exec);
        assert!(!options.print_handoff);
    }

    #[test]
    fn core_runner_api_executes_through_the_registered_local_adapter() {
        homeboy_core::test_support::with_isolated_home(|_| {
            crate::create(r#"{"id":"runner-api-local","kind":"local"}"#, false)
                .expect("create local runner");
            super::register();
            let mut request = RunnerExecutionEnvelope::planned("exec-local", "runner_api");
            request.dispatch = Some(RunnerExecutionDispatch {
                runner_id: "runner-api-local".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf runner-api-ok".to_string(),
                ],
                cwd: None,
                env: HashMap::new(),
                source_snapshot: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
            });

            let record = homeboy_core::runner::submit(request).expect("submit through core API");

            assert_eq!(record.runner_id, "runner-api-local");
            assert_eq!(record.transport, "local");
            assert_eq!(record.status, "succeeded");
            assert!(record.orchestration_provenance.is_some());
        });
    }
}
