//! Execute-API idempotency claims, kept as coordination records in the
//! observation store (#15947). The first caller for a key creates the record
//! `in_progress` through a revision compare-and-set; completing it stores the
//! response so a replay of the same request returns it unchanged.

use homeboy_core::observation::coordination::{
    CoordinationRecord, ExpectedRevision, RecordOutcome, RecordWrite,
};
use homeboy_core::observation::ObservationStore;
use homeboy_core::paths::PathRoots;
use homeboy_extension_contract::api::v1::ExtensionApiExecuteResponse;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Coordination namespace owned by the execute API.
pub(super) const NAMESPACE: &str = "extension-execute-idempotency";

const STATE_IN_PROGRESS: &str = "in_progress";
const STATE_COMPLETED: &str = "completed";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IdempotencyPayload {
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response: Option<ExtensionApiExecuteResponse>,
}

#[derive(Debug)]
pub(super) enum IdempotencyClaim {
    Accepted,
    Replayed(ExtensionApiExecuteResponse),
    InProgress,
    Conflict,
}

pub(super) fn claim(
    idempotency_key: &str,
    fingerprint: &serde_json::Value,
) -> Result<IdempotencyClaim, String> {
    claim_in(&open_store()?, idempotency_key, fingerprint)
}

pub(super) fn complete(
    idempotency_key: &str,
    fingerprint: &serde_json::Value,
    response: &ExtensionApiExecuteResponse,
) -> Result<(), String> {
    complete_in(&open_store()?, idempotency_key, fingerprint, response)
}

fn claim_in(
    store: &ObservationStore,
    idempotency_key: &str,
    fingerprint: &serde_json::Value,
) -> Result<IdempotencyClaim, String> {
    let fingerprint = fingerprint_digest(fingerprint)?;
    let payload = to_payload(&IdempotencyPayload {
        fingerprint: fingerprint.clone(),
        response: None,
    })?;
    let outcome = store
        .put_coordination_record(
            NAMESPACE,
            idempotency_key,
            ExpectedRevision::Absent,
            RecordWrite {
                state: STATE_IN_PROGRESS,
                payload: &payload,
                expires_at_ms: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(match outcome {
        RecordOutcome::Applied(_) => IdempotencyClaim::Accepted,
        RecordOutcome::Conflict { current: None } => IdempotencyClaim::InProgress,
        RecordOutcome::Conflict {
            current: Some(record),
        } => existing_claim(&record, &fingerprint),
    })
}

fn complete_in(
    store: &ObservationStore,
    idempotency_key: &str,
    fingerprint: &serde_json::Value,
    response: &ExtensionApiExecuteResponse,
) -> Result<(), String> {
    let fingerprint = fingerprint_digest(fingerprint)?;
    let current = store
        .coordination_record(NAMESPACE, idempotency_key)
        .map_err(|error| error.to_string())?;
    if let Some(record) = &current {
        if parse_payload(record)?.fingerprint != fingerprint {
            return Err(format!(
                "idempotency key '{idempotency_key}' is claimed by a different execute request"
            ));
        }
    }
    let expected = current.map_or(ExpectedRevision::Absent, |record| {
        ExpectedRevision::Revision(record.revision)
    });
    let payload = to_payload(&IdempotencyPayload {
        fingerprint,
        response: Some(response.clone()),
    })?;
    match store
        .put_coordination_record(
            NAMESPACE,
            idempotency_key,
            expected,
            RecordWrite {
                state: STATE_COMPLETED,
                payload: &payload,
                expires_at_ms: None,
            },
        )
        .map_err(|error| error.to_string())?
    {
        RecordOutcome::Applied(_) => Ok(()),
        RecordOutcome::Conflict { .. } => Err(format!(
            "idempotency record for '{idempotency_key}' changed while completing it"
        )),
    }
}

fn existing_claim(record: &CoordinationRecord, fingerprint: &str) -> IdempotencyClaim {
    let Ok(payload) = parse_payload(record) else {
        return IdempotencyClaim::InProgress;
    };
    if payload.fingerprint != fingerprint {
        return IdempotencyClaim::Conflict;
    }
    match (record.state.as_str(), payload.response) {
        (STATE_COMPLETED, Some(response)) => IdempotencyClaim::Replayed(response),
        _ => IdempotencyClaim::InProgress,
    }
}

fn open_store() -> Result<ObservationStore, String> {
    let roots = PathRoots::from_environment().map_err(|error| error.to_string())?;
    ObservationStore::open_initialized_for_lifecycle_in_roots(&roots)
        .map_err(|error| error.to_string())
}

fn parse_payload(record: &CoordinationRecord) -> Result<IdempotencyPayload, String> {
    serde_json::from_value(record.payload.clone()).map_err(|error| error.to_string())
}

fn to_payload(payload: &IdempotencyPayload) -> Result<serde_json::Value, String> {
    serde_json::to_value(payload).map_err(|error| error.to_string())
}

fn fingerprint_digest(fingerprint: &serde_json::Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(fingerprint).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use homeboy_extension_contract::api::v1::ExtensionApiExecuteState;
    use serde_json::json;

    use super::*;

    fn isolated_store() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("observations.sqlite");
        drop(ObservationStore::open_initialized_for_lifecycle_at(&path).expect("create store"));
        (directory, path)
    }

    fn open(path: &std::path::Path) -> ObservationStore {
        ObservationStore::open_initialized_for_lifecycle_at(path).expect("open store")
    }

    fn response(stdout: &str) -> ExtensionApiExecuteResponse {
        use homeboy_extension_contract::api::v1::{
            ExtensionApiExecuteOutput, EXTENSION_API_EXECUTE_RESPONSE_SCHEMA, EXTENSION_API_V1,
        };
        ExtensionApiExecuteResponse {
            schema: EXTENSION_API_EXECUTE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: Some("fixture".to_string()),
            project_id: None,
            control_plane: None,
            exit_code: Some(0),
            output: Some(ExtensionApiExecuteOutput {
                stdout: stdout.to_string(),
                stderr: String::new(),
            }),
            state: Some(ExtensionApiExecuteState::Completed),
            failure: None,
        }
    }

    #[test]
    fn concurrent_first_claims_accept_exactly_one_caller() {
        const CALLERS: usize = 6;
        let (_dir, path) = isolated_store();
        let barrier = Arc::new(Barrier::new(CALLERS));
        let threads: Vec<_> = (0..CALLERS)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let store = open(&path);
                    barrier.wait();
                    claim_in(&store, "key", &json!({"argv": ["a"]})).expect("claim")
                })
            })
            .collect();
        let claims: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, IdempotencyClaim::Accepted))
                .count(),
            1
        );
        assert!(claims.iter().all(|claim| matches!(
            claim,
            IdempotencyClaim::Accepted | IdempotencyClaim::InProgress
        )));
        let record = open(&path)
            .coordination_record(NAMESPACE, "key")
            .unwrap()
            .expect("claimed record");
        assert_eq!(record.state, STATE_IN_PROGRESS);
    }

    #[test]
    fn a_different_request_under_the_same_key_conflicts() {
        let (_dir, path) = isolated_store();
        let store = open(&path);
        assert!(matches!(
            claim_in(&store, "key", &json!({"argv": ["a"]})).unwrap(),
            IdempotencyClaim::Accepted
        ));
        assert!(matches!(
            claim_in(&store, "key", &json!({"argv": ["b"]})).unwrap(),
            IdempotencyClaim::Conflict
        ));
        assert!(complete_in(&store, "key", &json!({"argv": ["b"]}), &response("b")).is_err());
        let record = store
            .coordination_record(NAMESPACE, "key")
            .unwrap()
            .unwrap();
        assert_eq!(
            record.state, STATE_IN_PROGRESS,
            "a conflict must not complete"
        );
    }

    #[test]
    fn a_completed_request_replays_its_stored_response() {
        let (_dir, path) = isolated_store();
        let store = open(&path);
        let fingerprint = json!({"argv": ["a"]});
        assert!(matches!(
            claim_in(&store, "key", &fingerprint).unwrap(),
            IdempotencyClaim::Accepted
        ));
        assert!(matches!(
            claim_in(&store, "key", &fingerprint).unwrap(),
            IdempotencyClaim::InProgress
        ));
        let stored = response("out");
        complete_in(&store, "key", &fingerprint, &stored).unwrap();

        let record = open(&path)
            .coordination_record(NAMESPACE, "key")
            .unwrap()
            .unwrap();
        assert_eq!(record.state, STATE_COMPLETED);
        assert_eq!(record.revision, 2);
        match claim_in(&open(&path), "key", &fingerprint).unwrap() {
            IdempotencyClaim::Replayed(replayed) => {
                assert_eq!(replayed, stored);
                assert_eq!(replayed.state, Some(ExtensionApiExecuteState::Completed));
            }
            other => panic!("expected a replay, got {other:?}"),
        }
    }
}
