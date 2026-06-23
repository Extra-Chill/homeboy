use super::*;

pub enum RunnerTarget {
    Local {
        id: String,
        runner: Option<Runner>,
    },
    Ssh {
        id: String,
        runner: Runner,
        server: Server,
        client: SshClient,
    },
}

pub fn resolve(runner_id: &str) -> homeboy::core::Result<RunnerTarget> {
    match runner::load(runner_id) {
        Ok(runner) => from_registry(runner_id, runner),
        Err(_) if is_local_runner_id(runner_id) => Ok(RunnerTarget::Local {
            id: runner_id.to_string(),
            runner: None,
        }),
        Err(err) => Err(err),
    }
}

fn from_registry(runner_id: &str, runner: Runner) -> homeboy::core::Result<RunnerTarget> {
    match runner.kind {
        RunnerKind::Local => Ok(RunnerTarget::Local {
            id: runner_id.to_string(),
            runner: Some(runner),
        }),
        RunnerKind::Ssh => {
            let server_id = runner.server_id.as_deref().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "server_id",
                    "SSH runners require server_id",
                    None,
                    None,
                )
            })?;
            let server = server::load(server_id)?;
            let mut client = SshClient::from_server(&server, server_id)?;
            client.env.extend(runner.env.clone());
            Ok(RunnerTarget::Ssh {
                id: runner_id.to_string(),
                runner,
                server,
                client,
            })
        }
    }
}

fn is_local_runner_id(runner_id: &str) -> bool {
    matches!(runner_id, "local" | "localhost" | "self")
}
