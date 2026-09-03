use super::*;

pub enum RunnerTarget {
    Local {
        id: String,
        runner: Option<Runner>,
    },
    Ssh {
        id: String,
        runner: Runner,
        server: Box<Server>,
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

impl RunnerTarget {
    /// Lifecycle operations still accept a runner id and therefore reload its
    /// registry entry. Refuse that operation if the entry no longer names the
    /// target diagnosed at the start of this doctor invocation.
    pub fn ensure_current(&self) -> homeboy::core::Result<()> {
        let Self::Ssh {
            id, runner, server, ..
        } = self
        else {
            return Ok(());
        };
        let current = resolve(id)?;
        let Self::Ssh {
            runner: current_runner,
            server: current_server,
            ..
        } = current
        else {
            return Err(target_changed(id));
        };
        if same_identity(runner, &current_runner) && same_identity(server.as_ref(), &current_server)
        {
            Ok(())
        } else {
            Err(target_changed(id))
        }
    }
}

fn same_identity<T: serde::Serialize>(left: &T, right: &T) -> bool {
    serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
}

fn target_changed(runner_id: &str) -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "runner",
        "runner registry target changed during doctor; no repair was applied to a different target",
        Some(runner_id.to_string()),
        None,
    )
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
                server: Box::new(server),
                client,
            })
        }
    }
}

fn is_local_runner_id(runner_id: &str) -> bool {
    matches!(runner_id, "local" | "localhost" | "self")
}
