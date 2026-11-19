//! `agent-task history --task-url`: attempt history as data (#18451). The
//! same page is served over REST at `GET /v1/control-plane/task-history`.

use serde_json::Value;

use super::HistoryArgs;
use crate::commands::CmdResult;
use homeboy::agents::agent_task_service::{task_history, AgentTaskHistoryOptions};

pub(crate) fn history(args: HistoryArgs) -> CmdResult<Value> {
    let page = task_history(AgentTaskHistoryOptions {
        task_url: args.task_url,
        limit: args.limit,
        cursor: args.cursor,
    })?;
    Ok((serde_json::to_value(page).unwrap_or(Value::Null), 0))
}

struct TaskHistoryResource;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskHistoryQuery {
    task_url: String,
    #[serde(default)]
    limit: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
}

impl homeboy::core::control_plane::ControlPlaneReadResourceProvider for TaskHistoryResource {
    fn resource_type(&self) -> &'static str {
        "task-history"
    }

    fn read(&self, query: &Value) -> homeboy::core::Result<Value> {
        let invalid = |message: String| {
            homeboy::core::Error::validation_invalid_argument(
                "task-history.query",
                message,
                None,
                None,
            )
        };
        let query: TaskHistoryQuery =
            serde_json::from_value(query.clone()).map_err(|error| invalid(error.to_string()))?;
        let limit = query
            .limit
            .map(|limit| limit.parse::<usize>())
            .transpose()
            .map_err(|error| invalid(format!("limit: {error}")))?
            .unwrap_or(20);
        history(HistoryArgs {
            task_url: query.task_url,
            limit,
            cursor: query.cursor,
        })
        .map(|(page, _)| page)
    }
}

pub(crate) fn register_task_history_resource() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        homeboy::core::control_plane::register_read_resource_provider(std::sync::Arc::new(
            TaskHistoryResource,
        ));
    });
}
