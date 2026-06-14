use homeboy::core::runners as runner;
use homeboy::core::Error;

use super::{CmdResult, RunsListArgs, RunsListOutput, RunsOutput};

pub fn list_runner_runs(
    runner_id: &str,
    args: RunsListArgs,
    command: &'static str,
) -> CmdResult<RunsOutput> {
    let mut query = Vec::new();
    if let Some(kind) = args.kind {
        query.push(("kind", kind));
    }
    if let Some(component_id) = args.component_id {
        query.push(("component", component_id));
    }
    if let Some(status) = args.status {
        query.push(("status", status));
    }
    if let Some(rig) = args.rig {
        query.push(("rig", rig));
    }
    query.push(("limit", args.limit.to_string()));
    let query = query
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, url_encode_component(&value)))
        .collect::<Vec<_>>()
        .join("&");
    let data = runner::daemon_api_get(runner_id, &format!("/runs?{query}"))?;
    let runs = serde_json::from_value(data["body"]["runs"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner daemon runs list".to_string()),
        )
    })?;

    Ok((RunsOutput::List(RunsListOutput { command, runs }), 0))
}

fn url_encode_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
