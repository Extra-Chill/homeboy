//! Data-driven command status contract checks.

use std::path::Path;

use crate::core::component::CommandStatusContractConfig;

use super::super::{AuditFinding, Finding, Severity};

pub(in crate::core::code_audit) fn run(
    root: &Path,
    config: &CommandStatusContractConfig,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    for scenario in &config.scenarios {
        let fixture = root.join(&scenario.file);
        let Ok(content) = std::fs::read_to_string(&fixture) else {
            findings.push(finding(
                &scenario.file,
                &scenario.id,
                "scenario fixture is missing or unreadable".to_string(),
                "Write the scenario fixture or remove the stale command_status_contracts entry."
                    .to_string(),
            ));
            continue;
        };

        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            findings.push(finding(
                &scenario.file,
                &scenario.id,
                "scenario fixture is not valid JSON".to_string(),
                "Store command status scenario fixtures as JSON output envelopes.".to_string(),
            ));
            continue;
        };

        for (pointer, expected) in &scenario.expected_fields {
            match json.pointer(pointer) {
                Some(actual) if actual == expected => {}
                Some(actual) => findings.push(finding(
                    &scenario.file,
                    &scenario.id,
                    format!(
                        "expected {pointer} to be {}, found {}",
                        json_value_label(expected),
                        json_value_label(actual)
                    ),
                    format!(
                        "Update the command implementation or fixture so scenario '{}' reports consistent no-op/dry-run status semantics.",
                        scenario.id
                    ),
                )),
                None => findings.push(finding(
                    &scenario.file,
                    &scenario.id,
                    format!("expected field {pointer} is missing"),
                    format!(
                        "Expose {pointer} in scenario '{}' or remove the stale expectation.",
                        scenario.id
                    ),
                )),
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn finding(file: &str, scenario_id: &str, description: String, suggestion: String) -> Finding {
    Finding {
        convention: "command_status_contracts".to_string(),
        severity: Severity::Warning,
        file: file.to_string(),
        description: format!(
            "Command status scenario '{scenario_id}' violates contract: {description}"
        ),
        suggestion,
        kind: AuditFinding::CommandStatusContractViolation,
    }
}

fn json_value_label(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::CommandStatusContractScenario;
    use std::collections::BTreeMap;

    #[test]
    fn matching_fixture_is_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("no-match.json"),
            r#"{"success":true,"data":{"total_replacements":0,"written":false}}"#,
        )
        .expect("fixture");
        let config = CommandStatusContractConfig {
            scenarios: vec![scenario(
                "refactor-transform-no-match",
                "no-match.json",
                [
                    ("/success", serde_json::json!(true)),
                    ("/data/total_replacements", serde_json::json!(0)),
                    ("/data/written", serde_json::json!(false)),
                ],
            )],
        };

        assert!(run(dir.path(), &config).is_empty());
    }

    #[test]
    fn mismatched_fixture_reports_field_and_scenario() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("no-match.json"),
            r#"{"success":false,"data":{"total_replacements":0,"written":false}}"#,
        )
        .expect("fixture");
        let config = CommandStatusContractConfig {
            scenarios: vec![scenario(
                "refactor-transform-no-match",
                "no-match.json",
                [("/success", serde_json::json!(true))],
            )],
        };

        let findings = run(dir.path(), &config);

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            AuditFinding::CommandStatusContractViolation
        );
        assert!(findings[0]
            .description
            .contains("refactor-transform-no-match"));
        assert!(findings[0].description.contains("/success"));
    }

    fn scenario<const N: usize>(
        id: &str,
        file: &str,
        expected: [(&str, serde_json::Value); N],
    ) -> CommandStatusContractScenario {
        CommandStatusContractScenario {
            id: id.to_string(),
            file: file.to_string(),
            expected_fields: expected
                .into_iter()
                .map(|(pointer, value)| (pointer.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
        }
    }
}
