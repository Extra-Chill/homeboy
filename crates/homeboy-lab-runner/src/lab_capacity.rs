//! Read-only capacity observations from the execution host, not the controller.
use serde_json::{json, Value};

pub fn report(runner_id: &str) -> homeboy_core::Result<Value> {
    let runner = crate::load(runner_id)?;
    let status = crate::status(runner_id)?;
    let url = status
        .session
        .as_ref()
        .and_then(|session| session.local_url.as_deref())
        .ok_or_else(|| {
            homeboy_core::error::Error::internal_unexpected(
                "runner capacity unavailable: no daemon transport",
            )
        })?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|error| homeboy_core::error::Error::internal_unexpected(error.to_string()))?;
    let response = crate::daemon_http_get::daemon_get(&client, url, "/capacity")?;
    let mut body = response
        .pointer("/data/body")
        .or_else(|| response.get("body"))
        .unwrap_or(&response)
        .clone();
    if let Some(capacity) = body.get_mut("capacity") {
        let cpus = capacity["cpus"].as_u64().unwrap_or(1) as usize;
        let limit = runner.settings.concurrency_limit;
        capacity["concurrency_limit"] = json!(limit);
        capacity["effective_budget"] = json!(homeboy_core::lab_capacity::admission_budget(
            cpus,
            capacity["load_average_1m"].as_f64(),
            limit.unwrap_or(usize::MAX)
        ));
    }
    Ok(json!({"runner_id": runner_id, "observation": body}))
}
