use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::core::finding::{FindingSource, HomeboyFinding};

pub(crate) fn deserialize_budget_findings<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<HomeboyFinding>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<HomeboyFinding>::deserialize(deserializer)?;
    Ok(values)
}

pub(crate) fn failure(
    code: impl Into<String>,
    context_label: impl Into<String>,
    message: impl Into<String>,
    actual: impl Into<Option<f64>>,
    expected: f64,
    unit: impl Into<String>,
    subject: Option<String>,
) -> HomeboyFinding {
    let code = code.into();
    let context_label = context_label.into();
    let actual = actual.into();
    let unit = unit.into();
    let fingerprint = match subject.as_deref() {
        Some(subject) if !subject.is_empty() => format!("{}:{}", code, subject),
        _ => code.clone(),
    };

    HomeboyFinding::builder("budget", message.into())
        .rule(code.clone())
        .category("budget")
        .severity("error")
        .fingerprint(fingerprint)
        .source(FindingSource::new("budget").label(context_label.clone()))
        .metadata("code", code)
        .metadata("category", "budget")
        .metadata("context_label", context_label)
        .metadata("actual", actual)
        .metadata("expected", expected)
        .metadata("unit", unit)
        .metadata("subject", subject.clone())
        .metadata("passed", false)
        .build()
}

pub(crate) fn is_gate_failure(finding: &HomeboyFinding) -> bool {
    finding.severity.as_deref() == Some("error")
        || finding.metadata.get("passed").and_then(Value::as_bool) == Some(false)
}

#[cfg(test)]
mod tests {
    use super::super::gate::evaluate_gates;
    use super::super::parsing::parse_bench_results_str;

    #[test]
    fn emitted_budget_findings_gate_bench_runs() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "budget_findings": [
                {
                    "tool": "budget",
                    "rule": "rest.max_response_bytes",
                    "category": "budget",
                    "severity": "error",
                    "message": "REST response exceeded 250 KB budget",
                    "fingerprint": "rest.max_response_bytes:/wp-json/datamachine/v1/pipelines?per_page=100",
                    "source": { "kind": "budget", "label": "profile:wordpress-rest" },
                    "metadata": {
                        "code": "rest.max_response_bytes",
                        "category": "budget",
                        "context_label": "profile:wordpress-rest",
                        "actual": 4378195,
                        "expected": 250000,
                        "unit": "bytes",
                        "subject": "/wp-json/datamachine/v1/pipelines?per_page=100",
                        "passed": false
                    }
                }
            ],
            "scenarios": [{ "id": "wordpress-rest", "iterations": 1, "metrics": { "p95_ms": 50.0 } }]
        }"#;

        let mut parsed = parse_bench_results_str(raw).unwrap();
        let failures = evaluate_gates(&mut parsed);

        assert_eq!(failures, vec!["REST response exceeded 250 KB budget"]);
        assert_eq!(parsed.budget_findings[0].metadata["actual"], 4378195.0);
        assert_eq!(parsed.budget_findings[0].metadata["expected"], 250000.0);
        assert_eq!(parsed.budget_findings[0].metadata["unit"], "bytes");
    }
}
