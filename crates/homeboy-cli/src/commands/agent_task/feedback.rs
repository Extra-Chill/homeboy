use std::io::Read;

use serde_json::Value;

use super::args::CookFeedbackArgs;
use super::CmdResult;

pub(crate) fn feedback(args: CookFeedbackArgs) -> CmdResult<Value> {
    let store = homeboy::agents::agent_task_feedback::CookFeedbackStore::from_current_data_root()?;
    if args.status {
        return Ok((store.status_value(&args.cook_id)?, 0));
    }
    let candidate = args.candidate.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "candidate",
            "--candidate is required when submitting feedback",
            None,
            None,
        )
    })?;
    let idempotency_key = args.idempotency_key.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "idempotency-key",
            "--idempotency-key is required when submitting feedback",
            None,
            None,
        )
    })?;
    let text = match (args.text, args.file, args.stdin) {
        (Some(text), None, false) => text,
        (None, Some(path), false) => std::fs::read_to_string(&path)
            .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), Some(path)))?,
        (None, None, true) => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
            text
        }
        _ => {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "cook_feedback.text",
                "provide exactly one of --text, --file, or --stdin",
                None,
                None,
            ))
        }
    };
    let (feedback, created) = store.submit(
        &args.cook_id,
        &candidate,
        &args.author,
        &args.source,
        &text,
        &idempotency_key,
    )?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-feedback-result/v1",
            "created": created,
            "feedback": feedback,
        }),
        0,
    ))
}
