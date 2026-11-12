//! Canonical durable events, independent of the observation store.

use clap::Args;
use serde_json::Value;

use super::CmdResult;

#[derive(Args, Debug, Clone)]
pub(crate) struct RunsLogsArgs {
    /// Durable run or Cook ID whose logs to retrieve.
    pub run_id: String,
    /// Resume events after this opaque cursor.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}

pub(crate) fn logs(args: RunsLogsArgs) -> CmdResult<Value> {
    let store = homeboy::core::observation::ObservationStore::open_initialized()?;
    if let Some(report) = super::gate_output::report(&store, &args.run_id) {
        return Ok((
            serde_json::json!({
                "run_id": args.run_id,
                "stdout": report.get("stdout"),
                "stderr": report.get("stderr"),
                "artifact": report.pointer("/lab_receipt/artifact_ref"),
            }),
            0,
        ));
    }
    let cursor = args
        .cursor
        .as_deref()
        .map(homeboy_control_plane_contract::EventCursor::new)
        .transpose()
        .map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "cursor",
                error.to_string(),
                args.cursor.clone(),
                None,
            )
        })?;
    let events =
        homeboy::agents::agent_task_service::logs_from_cursor(&args.run_id, cursor.as_ref())?;
    Ok((serde_json::to_value(events).unwrap_or(Value::Null), 0))
}
