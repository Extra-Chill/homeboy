use std::collections::BTreeMap;

use homeboy::core::runners::{self as runner};
use homeboy::core::server::RunnerSecretEnvRef;

use super::super::CmdResult;
use super::status::declared_tool_diagnostics_for_legacy;
use super::types::{
    RunnerEnvDiagnostics, RunnerEnvOutput, RunnerSecretEnvReferenceOutput, REDACTED_ENV_VALUE,
};

pub(super) fn env(runner_id: &str) -> CmdResult<RunnerEnvOutput> {
    let runner = runner::load(runner_id)?;
    let effective_env = runner::effective_env(runner_id)?;
    let diagnostic_env = effective_env.into_iter().collect::<BTreeMap<_, _>>();
    let env = diagnostic_env
        .keys()
        .cloned()
        .map(|key| (key, REDACTED_ENV_VALUE.to_string()))
        .collect();

    let wp_codebox =
        declared_tool_diagnostics_for_legacy("wp_codebox", Some(runner_id), &diagnostic_env);

    let secret_env = runner
        .secret_env
        .into_iter()
        .map(|(key, reference)| (key, secret_env_reference_output(reference)))
        .collect();

    Ok((
        RunnerEnvOutput {
            variant: "env",
            command: "runner.env".to_string(),
            runner_id: runner_id.to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: true,
            env,
            secret_env,
            diagnostics: RunnerEnvDiagnostics {
                server_shell_env: "Use `homeboy ssh <server> -- printenv NAME` to inspect the server login shell environment; it does not include runner job env by default.".to_string(),
                runner_job_env: "This output shows configured public env Homeboy injects into runner jobs. secret_env entries are shown as refs only; their values resolve on the runner at execution time and are never printed here.".to_string(),
                wp_codebox,
            },
        },
        0,
    ))
}

fn secret_env_reference_output(reference: RunnerSecretEnvRef) -> RunnerSecretEnvReferenceOutput {
    RunnerSecretEnvReferenceOutput {
        env: reference.env,
        file: reference.file,
        secret: reference.secret,
        values_redacted: true,
    }
}
