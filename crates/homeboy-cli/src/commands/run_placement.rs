//! Operator-facing execution location, never inferred from requested policy.
use serde_json::{json, Value};

pub(crate) fn placement(run: &Value) -> Value {
    let mut projected = location(run);
    if let Some(fields) = run["placement"].as_object() {
        for (key, value) in fields {
            projected
                .as_object_mut()
                .unwrap()
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
    projected
}

fn location(run: &Value) -> Value {
    let existing = &run["placement"];
    if matches!(existing["kind"].as_str(), Some("lab" | "local" | "pending")) {
        return existing.clone();
    }
    let selected = existing["effective"]
        .as_str()
        .or(existing["selected"].as_str());
    let runner = existing["runner_id"]
        .as_str()
        .or_else(|| run["runner_id"].as_str())
        .or_else(|| {
            (run["owner"]["kind"] == "runner")
                .then(|| run["owner"]["id"].as_str())
                .flatten()
        })
        .or_else(|| {
            run.pointer("/metadata/execution_placement_outcome/runner_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            run.pointer("/metadata/execution_placement_decision/runner/runner_id")
                .and_then(Value::as_str)
        });
    if selected == Some("controller") {
        return json!({"kind": "local", "runner_id": null});
    }
    if let Some(runner) = runner.filter(|id| !id.trim().is_empty()) {
        return json!({"kind": "lab", "runner_id": runner});
    }
    if run
        .pointer("/metadata/execution_placement_decision/selected")
        .and_then(Value::as_str)
        == Some("local")
        || run
            .pointer("/metadata/execution_placement_outcome/effective")
            .and_then(Value::as_str)
            == Some("local")
    {
        return json!({"kind": "local", "runner_id": null});
    }
    json!({"kind": "pending", "runner_id": null, "reason": "execution placement not yet decided"})
}

pub(crate) fn line(run: &Value) -> String {
    let placement = placement(run);
    match placement["kind"].as_str() {
        Some("lab") => format!(
            "Placement: lab ({})",
            placement["runner_id"].as_str().unwrap_or("unknown")
        ),
        Some("local") => "Placement: local (controller)".into(),
        _ => format!(
            "Placement: pending ({})",
            placement["reason"]
                .as_str()
                .unwrap_or("execution placement not yet decided")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_keeps_decision_fields_and_names_the_location() {
        let run = json!({
            "placement": {"decision_id":"decision-1", "selected":"runner", "runner_id":"homeboy-lab"},
        });
        let projected = placement(&run);
        assert_eq!(projected["decision_id"], "decision-1");
        assert_eq!(projected["kind"], "lab");
        assert_eq!(projected["runner_id"], "homeboy-lab");
        assert_eq!(
            placement(&json!({"placement": projected.clone()})),
            projected
        );
    }

    #[test]
    fn location_uses_execution_not_requested_policy() {
        for (run, expected) in [
            (
                json!({"owner":{"kind":"runner","id":"homeboy-lab"}}),
                "Placement: lab (homeboy-lab)",
            ),
            (
                json!({"placement":{"selected":"controller"}}),
                "Placement: local (controller)",
            ),
            (
                json!({"placement":{"requested":"runner","selected":"runner","effective":"controller","runner_id":"lab"}}),
                "Placement: local (controller)",
            ),
            (
                json!({"placement":{"requested":"local"}}),
                "Placement: pending (execution placement not yet decided)",
            ),
        ] {
            assert_eq!(line(&run), expected);
            assert!(placement(&run).get("runner_id").is_some());
        }
    }
}
