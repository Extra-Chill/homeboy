//! Canonical control-plane identities for the agent-task status projection.

use homeboy_control_plane_contract::{
    resolve, AttemptId, IdentityKind, MissionId, ResolveError, RunId,
};
use serde::{Deserialize, Serialize};

use super::AgentTaskRunRecord;
use homeboy_core::{Error, Result};

/// Typed identities `agent-task status` reports beside the existing `run_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalControlPlaneIdentities {
    pub mission: MissionId,
    pub run: RunId,
    pub attempt: AttemptId,
    pub attempt_number: u32,
}

/// Resolve the durable run through the control-plane contract.
///
/// A run id that does not encode an attempt is omitted rather than guessed.
/// When the durable record already carries `cook_attempt`, a disagreement with
/// the encoded attempt number is a typed error rather than a silent preference.
pub fn canonical_control_plane_identities(
    record: &AgentTaskRunRecord,
) -> Result<Option<CanonicalControlPlaneIdentities>> {
    canonical_control_plane_identities_for_run(&record.run_id, recorded_cook_attempt(record))
}

pub fn canonical_control_plane_identities_for_run(
    run_id: &str,
    recorded_attempt: Option<u32>,
) -> Result<Option<CanonicalControlPlaneIdentities>> {
    let resolved = match resolve(IdentityKind::RunId, run_id) {
        Ok(resolved) => resolved,
        Err(ResolveError::MalformedRun { .. }) => return Ok(None),
        Err(error) => {
            return Err(Error::validation_invalid_argument(
                "run_id",
                error.to_string(),
                Some(run_id.to_string()),
                None,
            ))
        }
    };
    let (Some(mission), Some(run), Some(attempt), Some(attempt_number)) = (
        resolved.mission,
        resolved.run,
        resolved.attempt,
        resolved.attempt_number,
    ) else {
        return Ok(None);
    };
    if let Some(recorded) = recorded_attempt {
        if recorded != attempt_number {
            return Err(Error::validation_invalid_argument(
                "cook_attempt",
                format!(
                    "run id `{run_id}` encodes attempt {attempt_number}, but the durable record carries attempt {recorded}"
                ),
                Some(run_id.to_string()),
                None,
            ));
        }
    }
    Ok(Some(CanonicalControlPlaneIdentities {
        mission,
        run,
        attempt,
        attempt_number,
    }))
}

fn recorded_cook_attempt(record: &AgentTaskRunRecord) -> Option<u32> {
    record
        .metadata
        .get("cook_attempt")
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_control_plane_identities, canonical_control_plane_identities_for_run,
        CanonicalControlPlaneIdentities,
    };
    use crate::agent_task_lifecycle::AgentTaskRunRecord;
    use serde_json::json;

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";

    fn record(run_id: &str, cook_attempt: Option<u32>) -> AgentTaskRunRecord {
        let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": run_id,
            "plan_id": "plan",
            "state": "queued",
            "submitted_at": "2026-01-01T00:00:00Z",
            "plan_path": "/plan"
        }))
        .expect("record");
        if let Some(attempt) = cook_attempt {
            record.metadata = json!({ "cook_attempt": attempt });
        }
        record
    }

    #[test]
    fn real_agent_task_run_id_resolves_mission_run_and_attempt() {
        let identities = canonical_control_plane_identities(&record(AGENT_TASK_RUN, Some(1)))
            .expect("resolve")
            .expect("canonical identities");
        assert_eq!(identities.mission.as_str(), AGENT_TASK_COOK);
        assert_eq!(identities.run.as_str(), AGENT_TASK_RUN);
        assert_eq!(identities.attempt.as_str(), AGENT_TASK_RUN);
        assert_eq!(identities.attempt_number, 1);
        let json = serde_json::to_value(&identities).expect("serialize");
        assert_eq!(json["mission"], AGENT_TASK_COOK);
        assert_eq!(json["run"], AGENT_TASK_RUN);
        assert_eq!(json["attempt"], AGENT_TASK_RUN);
        assert_eq!(json["attempt_number"], 1);
        let decoded: CanonicalControlPlaneIdentities =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(decoded, identities);
    }

    #[test]
    fn encoded_attempt_must_match_the_durable_record() {
        let error = canonical_control_plane_identities(&record(AGENT_TASK_RUN, Some(2)))
            .expect_err("disagreement");
        assert!(error.message.contains("encodes attempt 1"));
        assert!(error.message.contains("durable record carries attempt 2"));
    }

    #[test]
    fn malformed_run_id_is_omitted_rather_than_guessed() {
        assert_eq!(
            canonical_control_plane_identities_for_run("run-1", None).expect("omit"),
            None
        );
        assert_eq!(
            canonical_control_plane_identities_for_run(AGENT_TASK_COOK, None).expect("omit"),
            None
        );
    }
}
