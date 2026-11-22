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
    apply_limit(&mut body, runner.settings.concurrency_limit);
    Ok(json!({"runner_id": runner_id, "observation": body}))
}

/// Project the runner configuration onto the host snapshot, including its queue.
fn apply_limit(body: &mut Value, limit: Option<usize>) {
    let Some(capacity) = body.get_mut("capacity") else {
        return;
    };
    let cpus = capacity["cpus"].as_u64().unwrap_or(1) as usize;
    let budget = homeboy_core::lab_capacity::admission_budget(cpus, limit);
    capacity["concurrency_limit"] = json!(limit);
    capacity["effective_budget"] = json!(budget);
    if let Some(queue) = body.get_mut("step_queue") {
        queue["effective_budget"] = json!(budget);
        let share = queue["owner_max_share_percent"].as_u64().unwrap_or(50) as usize;
        queue["owner_slot_cap"] = json!(homeboy_core::lab_capacity::owner_slot_cap(budget, share));
        let etas: Vec<_> = queue["waiting"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|waiter| {
                homeboy_core::lab_capacity::eta_from_step_queue(
                    queue,
                    waiter["queue_position"].as_u64().unwrap_or(1) as usize,
                )
            })
            .collect();
        if let Some(waiters) = queue["waiting"].as_array_mut() {
            for (waiter, eta) in waiters.iter_mut().zip(etas) {
                waiter["eta_seconds"] = json!(eta);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_and_queue_share_configured_budget_despite_self_load() {
        for (limit, expected) in [(Some(24), 24), (None, 32)] {
            let mut snapshot = json!({
                "capacity": {"cpus": 32, "load_average_1m": 80.0, "effective_budget": 1},
                "step_queue": {"effective_budget": 9, "owner_max_share_percent": 50,
                    "holders": [], "waiting": []}
            });
            apply_limit(&mut snapshot, limit);
            assert_eq!(snapshot["capacity"]["effective_budget"], expected);
            assert_eq!(snapshot["step_queue"]["effective_budget"], expected);
            assert_eq!(snapshot["step_queue"]["owner_slot_cap"], expected / 2);
        }
    }
}
