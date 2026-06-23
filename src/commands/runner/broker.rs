use homeboy::core::runners::{self as runner};

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
