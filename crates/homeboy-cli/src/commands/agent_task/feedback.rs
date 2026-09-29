use std::io::Read;

use serde_json::Value;

use super::args::CookFeedbackArgs;
use super::CmdResult;

pub(crate) fn feedback(args: CookFeedbackArgs) -> CmdResult<Value> {
    let store = homeboy::agents::agent_task_feedback::CookFeedbackStore::from_current_data_root()?;
    if args.status {
        return Ok((store.status_value(&args.cook_id)?, 0));
    }
    let selection = homeboy::agents::agent_task_service::select_cook_candidate(&args.cook_id)?;
    if selection.incomplete || selection.run_id.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "candidate",
            "Cook candidate selection is incomplete; inspect `agent-task status` and retry",
            Some(serde_json::to_string(&selection).unwrap_or_default()),
            None,
        ));
    }
    let selected_identity = homeboy::agents::agent_task_lifecycle::exact_record(&selection.run_id)
        .ok()
        .and_then(|record| homeboy::agents::agent_task_feedback::candidate_identity(&record));
    let candidate = match args.candidate {
        Some(candidate) => candidate,
        None => selected_identity.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "candidate",
                "could not derive a canonical candidate identity; pass --candidate explicitly",
                Some(serde_json::to_string(&selection).unwrap_or_default()),
                None,
            )
        })?,
    };
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
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    let idempotency_key = args.idempotency_key.unwrap_or_else(|| {
        let material = serde_json::json!({
            "schema": "homeboy/cook-feedback-idempotency/v1",
            "cook_id": args.cook_id,
            "candidate": candidate,
            "author": args.author,
            "source": args.source,
            "text": normalized_text,
        });
        format!(
            "feedback-{}",
            homeboy_engine_primitives::content_hash::sha256_hex(
                serde_json::to_string(&material)
                    .unwrap_or_default()
                    .as_bytes()
            )
        )
    });
    let (feedback, created) = store.submit(
        &args.cook_id,
        &candidate,
        &args.author,
        &args.source,
        &normalized_text,
        &idempotency_key,
    )?;
    let recipe_store =
        homeboy::agents::agent_task_service::CookRecipeStore::from_current_data_root()?;
    let remediation_queued = if recipe_store.recipe_exists(&args.cook_id) {
        let lifecycle_store =
            homeboy::agents::agent_tasks::lifecycle::AgentTaskLifecycleStore::from_data_root(
                recipe_store.data_root(),
            );
        let run_id =
            homeboy::agents::agent_task_service::resolve_cook_continuation_run_id_in_store(
                &recipe_store,
                &lifecycle_store,
                &args.cook_id,
            )?;
        recipe_store.enqueue_feedback_remediation(&args.cook_id, &run_id)?
    } else {
        false
    };
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-feedback-result/v1",
            "created": created,
            "remediation_queued": remediation_queued,
            "feedback": feedback,
        }),
        0,
    ))
}
