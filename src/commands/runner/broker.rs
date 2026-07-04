use homeboy::core::engine::shell;
use homeboy::core::runners::{self as runner};
use homeboy::core::{server, Error};

use super::cli::RunnerBrokerCommand;
use super::types::{RunnerBrokerCredentialSummary, RunnerBrokerOutput};

pub(super) fn run_broker(
    command: RunnerBrokerCommand,
) -> Result<RunnerBrokerOutput, homeboy::core::Error> {
    use std::collections::BTreeSet;

    let mut store = runner::BrokerAuthStore::load()?;
    match command {
        RunnerBrokerCommand::Pair {
            id,
            runner_id,
            submit,
            work,
            no_install,
        } => {
            let mut scopes: BTreeSet<runner::BrokerScope> = BTreeSet::new();
            if submit {
                scopes.insert(runner::BrokerScope::Submit);
            }
            if work {
                scopes.insert(runner::BrokerScope::Work);
            }
            if scopes.is_empty() {
                // Default to a worker credential, the most common pairing.
                scopes.insert(runner::BrokerScope::Work);
            }
            let minted = store.pair(id, runner_id, scopes)?;
            if !no_install {
                install_store_on_ssh_runner(&minted.runner_id, &store)?;
            }
            let store_path = store.save()?;
            let scope_labels = scope_labels(&store, &minted.id);
            Ok(RunnerBrokerOutput {
                command: "runner.broker.pair",
                credential_id: Some(minted.id),
                runner_id: Some(minted.runner_id),
                scopes: scope_labels,
                token: Some(minted.token),
                revoked: None,
                credentials: Vec::new(),
                store_path: store_path.display().to_string(),
            })
        }
        RunnerBrokerCommand::Revoke { id } => {
            let revoked = store.revoke(&id);
            let store_path = store.save()?;
            Ok(RunnerBrokerOutput {
                command: "runner.broker.revoke",
                credential_id: Some(id),
                runner_id: None,
                scopes: Vec::new(),
                token: None,
                revoked: Some(revoked),
                credentials: Vec::new(),
                store_path: store_path.display().to_string(),
            })
        }
        RunnerBrokerCommand::List => {
            let credentials = store
                .credentials
                .iter()
                .map(|cred| RunnerBrokerCredentialSummary {
                    id: cred.id.clone(),
                    runner_id: cred.runner_id.clone(),
                    scopes: cred.scopes.iter().map(scope_label).collect(),
                    revoked: cred.revoked_at.is_some(),
                    created_at: cred.created_at.clone(),
                })
                .collect();
            // Listing does not mutate; resolve the path without rewriting.
            let path = runner::broker_auth_store_path()?;
            Ok(RunnerBrokerOutput {
                command: "runner.broker.list",
                credential_id: None,
                runner_id: None,
                scopes: Vec::new(),
                token: None,
                revoked: None,
                credentials,
                store_path: path.display().to_string(),
            })
        }
    }
}

fn install_store_on_ssh_runner(
    runner_id: &str,
    store: &runner::BrokerAuthStore,
) -> Result<(), Error> {
    let configured_runner = runner::load(runner_id)?;
    if configured_runner.kind != runner::RunnerKind::Ssh {
        return Ok(());
    }
    let server_id = configured_runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner_id",
            format!("SSH runner `{runner_id}` does not declare a server_id"),
            Some(runner_id.to_string()),
            None,
        )
    })?;
    let configured_server = server::load(server_id)?;
    let mut client = server::SshClient::from_server(&configured_server, server_id)?;
    client.env = runner::RunnerSpec::from_runner(&configured_runner).effective_env();

    let enforcement_store = store.enforcement_copy();
    let serialized = serde_json::to_string_pretty(&enforcement_store).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("serialize broker auth store for runner install".to_string()),
        )
    })?;
    let mut temp = tempfile::NamedTempFile::new().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create temporary broker auth store".to_string()),
        )
    })?;
    use std::io::Write;
    temp.write_all(serialized.as_bytes()).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write temporary broker auth store".to_string()),
        )
    })?;
    temp.flush().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("flush temporary broker auth store".to_string()),
        )
    })?;

    let remote_dir = ".config/homeboy";
    let remote_path = ".config/homeboy/broker_auth.json";
    let mkdir = client.execute(&format!("mkdir -p {}", shell::quote_path(remote_dir)));
    if !mkdir.success {
        return Err(Error::internal_io(
            mkdir.stderr.trim().to_string(),
            Some(format!(
                "create remote broker auth dir for runner `{runner_id}`"
            )),
        ));
    }
    let upload = client.upload_file(temp.path().to_string_lossy().as_ref(), remote_path);
    if !upload.success {
        return Err(Error::internal_io(
            upload.stderr.trim().to_string(),
            Some(format!("install broker auth store on runner `{runner_id}`")),
        ));
    }
    let chmod = client.execute(&format!("chmod 600 {}", shell::quote_path(remote_path)));
    if !chmod.success {
        return Err(Error::internal_io(
            chmod.stderr.trim().to_string(),
            Some(format!(
                "restrict broker auth store on runner `{runner_id}`"
            )),
        ));
    }
    Ok(())
}

fn scope_label(scope: &runner::BrokerScope) -> String {
    match scope {
        runner::BrokerScope::Submit => "submit".to_string(),
        runner::BrokerScope::Work => "work".to_string(),
    }
}

fn scope_labels(store: &runner::BrokerAuthStore, id: &str) -> Vec<String> {
    store
        .credentials
        .iter()
        .find(|cred| cred.id == id)
        .map(|cred| cred.scopes.iter().map(scope_label).collect())
        .unwrap_or_default()
}
