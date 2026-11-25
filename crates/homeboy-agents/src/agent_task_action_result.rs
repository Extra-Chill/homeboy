//! Typed agent-task views of canonical control-plane action acknowledgements.

use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionAcknowledgement, ControlPlaneActionOutcome,
    ControlPlaneActionPayload, CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
    CONTROL_PLANE_RESUME_RESULT_SCHEMA, CONTROL_PLANE_RETRY_RESULT_SCHEMA,
};
use homeboy_core::{Error, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::agent_task_lifecycle::AgentTaskRunRecord;

pub const UNMATERIALIZED_COOK_RESUME_RESULT_SCHEMA: &str = "homeboy/unmaterialized-cook-resume/v1";

#[derive(Debug, Clone, PartialEq)]
pub struct RetryActionResult {
    pub record: AgentTaskRunRecord,
    pub runnable: bool,
    pub created: bool,
    pub blocker: Option<Value>,
    pub recovered: bool,
}

pub fn retry(acknowledgement: &ControlPlaneActionAcknowledgement) -> Result<RetryActionResult> {
    validate(
        acknowledgement,
        ControlPlaneAction::Retry,
        CONTROL_PLANE_RETRY_RESULT_SCHEMA,
    )?;
    let result: RetryActionResultPayload = decode(acknowledgement, "retry")?;
    Ok(RetryActionResult {
        record: result.record,
        runnable: result.runnable,
        created: result.created,
        blocker: result.blocker,
        recovered: result.recovered,
    })
}

/// Project inline run continuation into the Recover report, preserving its
/// aggregate/acknowledgement distinction and terminal admission exit status.
pub fn resume_report(
    payload: &ControlPlaneActionPayload,
    outcome: ControlPlaneActionOutcome,
    acknowledgement: Value,
    message: Option<&str>,
) -> Result<(Value, i32)> {
    if outcome == ControlPlaneActionOutcome::Failed {
        return Err(Error::validation_invalid_argument(
            "action acknowledgement",
            message.unwrap_or("recover action failed"),
            None,
            None,
        ));
    }
    match payload.schema.as_str() {
        CONTROL_PLANE_RESUME_RESULT_SCHEMA => {
            let report = if outcome == ControlPlaneActionOutcome::AlreadySatisfied {
                payload.data["aggregate"].clone()
            } else {
                acknowledgement
            };
            Ok((
                report,
                payload.data["exit_code"].as_i64().unwrap_or(1) as i32,
            ))
        }
        UNMATERIALIZED_COOK_RESUME_RESULT_SCHEMA => Ok((
            acknowledgement,
            if payload.data["terminal"].as_bool().unwrap_or(false) {
                2
            } else {
                0
            },
        )),
        schema => Err(schema_error(
            "recover",
            CONTROL_PLANE_RESUME_RESULT_SCHEMA,
            schema,
        )),
    }
}

fn validate(
    acknowledgement: &ControlPlaneActionAcknowledgement,
    action: ControlPlaneAction,
    result_schema: &str,
) -> Result<()> {
    validate_action_and_outcome(acknowledgement, action)?;
    if acknowledgement.result.schema != result_schema {
        return Err(schema_error(
            action_name(action),
            result_schema,
            &acknowledgement.result.schema,
        ));
    }
    Ok(())
}

fn validate_action_and_outcome(
    acknowledgement: &ControlPlaneActionAcknowledgement,
    action: ControlPlaneAction,
) -> Result<()> {
    if acknowledgement.schema != CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA {
        return Err(schema_error(
            action_name(action),
            CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
            &acknowledgement.schema,
        ));
    }
    if acknowledgement.action != action {
        return Err(Error::validation_invalid_argument(
            "action acknowledgement",
            format!(
                "expected {} action acknowledgement, received {}",
                action_name(action),
                action_name(acknowledgement.action)
            ),
            None,
            None,
        ));
    }
    if acknowledgement.outcome == ControlPlaneActionOutcome::Failed {
        return Err(Error::validation_invalid_argument(
            "action acknowledgement",
            acknowledgement
                .message
                .clone()
                .unwrap_or_else(|| format!("{} action failed", action_name(action))),
            None,
            None,
        ));
    }
    Ok(())
}

fn decode<T>(acknowledgement: &ControlPlaneActionAcknowledgement, action: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(acknowledgement.result.data.clone()).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("decode {action} action result")),
        )
    })
}

fn schema_error(action: &str, expected: &str, actual: &str) -> Error {
    Error::validation_invalid_argument(
        "action acknowledgement",
        format!("{action} action result schema must be {expected}, received {actual}"),
        None,
        None,
    )
}

fn action_name(action: ControlPlaneAction) -> &'static str {
    match action {
        ControlPlaneAction::Cancel => "cancel",
        ControlPlaneAction::Reconcile => "reconcile",
        ControlPlaneAction::Recover => "recover",
        ControlPlaneAction::PlacementUpdate => "placement_update",
        ControlPlaneAction::Retry => "retry",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryActionResultPayload {
    record: AgentTaskRunRecord,
    runnable: bool,
    created: bool,
    #[serde(default)]
    blocker: Option<Value>,
    #[serde(default)]
    recovered: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::{ControlPlaneActionPayload, ControlPlaneRun, RunId};

    fn acknowledgement(
        action: ControlPlaneAction,
        schema: &str,
    ) -> ControlPlaneActionAcknowledgement {
        let run = RunId::new("run-1").expect("run");
        ControlPlaneActionAcknowledgement {
            schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
            acknowledgement: "ack-1".to_string(),
            run: run.clone(),
            action,
            idempotency_key: "key-1".to_string(),
            actor: "test".to_string(),
            accepted_at: "2026-01-01T00:00:00Z".to_string(),
            completed_at: "2026-01-01T00:00:01Z".to_string(),
            outcome: ControlPlaneActionOutcome::Succeeded,
            resource: ControlPlaneRun::new(run),
            result: ControlPlaneActionPayload {
                schema: schema.to_string(),
                data: Value::Null,
            },
            message: None,
        }
    }

    fn retry_acknowledgement() -> ControlPlaneActionAcknowledgement {
        let mut acknowledgement =
            acknowledgement(ControlPlaneAction::Retry, CONTROL_PLANE_RETRY_RESULT_SCHEMA);
        let record = AgentTaskRunRecord::new_queued(
            "retry-1".to_string(),
            "plan-1".to_string(),
            "plan.json".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        acknowledgement.result.data = serde_json::json!({
            "record": record,
            "runnable": true,
            "created": true,
        });
        acknowledgement
    }

    #[test]
    fn retry_decodes_every_producer_result_shape() {
        let blocker = serde_json::json!({
            "error": "scheduling daemon unavailable",
            "next_action": "homeboy daemon recover",
        });
        let cases: [(&str, Value, bool, bool, Option<Value>); 3] = [
            ("plain", serde_json::json!({}), true, false, None),
            (
                "scheduling blocker",
                serde_json::json!({ "blocker": blocker.clone() }),
                true,
                false,
                Some(blocker.clone()),
            ),
            (
                "recovered successor",
                serde_json::json!({ "created": false, "recovered": true }),
                false,
                true,
                None,
            ),
        ];
        for (label, overrides, created, recovered, expected_blocker) in cases {
            let mut acknowledgement = retry_acknowledgement();
            for (key, value) in overrides.as_object().expect("overrides") {
                acknowledgement.result.data[key] = value.clone();
            }

            let result = retry(&acknowledgement).expect(label);

            assert_eq!(
                serde_json::to_value(&result.record).unwrap(),
                acknowledgement.result.data["record"],
                "{label}"
            );
            assert!(result.runnable, "{label}");
            assert_eq!(result.created, created, "{label}");
            assert_eq!(result.recovered, recovered, "{label}");
            assert_eq!(result.blocker, expected_blocker, "{label}");
        }
    }

    #[test]
    fn retry_rejects_unknown_result_fields() {
        let mut acknowledgement = retry_acknowledgement();
        acknowledgement.result.data["unexpected"] = Value::Bool(true);

        let error = retry(&acknowledgement).expect_err("unknown field must be refused");

        assert!(error.details["error"]
            .as_str()
            .expect("JSON decode error detail")
            .contains("unknown field `unexpected`"));
    }

    #[test]
    fn retry_refuses_mismatched_or_failed_acknowledgements_before_decoding() {
        let mut failed =
            acknowledgement(ControlPlaneAction::Retry, CONTROL_PLANE_RETRY_RESULT_SCHEMA);
        failed.outcome = ControlPlaneActionOutcome::Failed;
        failed.message = Some("retry admission rejected".to_string());
        let cases = [
            (
                acknowledgement(
                    ControlPlaneAction::Recover,
                    CONTROL_PLANE_RETRY_RESULT_SCHEMA,
                ),
                "expected retry action acknowledgement",
            ),
            (
                acknowledgement(
                    ControlPlaneAction::Retry,
                    CONTROL_PLANE_RESUME_RESULT_SCHEMA,
                ),
                CONTROL_PLANE_RETRY_RESULT_SCHEMA,
            ),
            (failed, "retry admission rejected"),
        ];
        for (acknowledgement, expected) in cases {
            let error = retry(&acknowledgement).expect_err(expected);
            assert!(error.message.contains(expected), "{}", error.message);
        }
    }
}
