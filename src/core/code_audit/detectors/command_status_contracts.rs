//! Data-driven command status contract checks.

use std::path::Path;

use crate::core::component::{CommandStatusContractConfig, CommandStatusContractScenario};

use super::super::{AuditFinding, Finding, Severity};

pub(in crate::core::code_audit) fn run(
    root: &Path,
    config: &CommandStatusContractConfig,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    for command in &config.required_commands {
        if !config
            .scenarios
            .iter()
            .any(|scenario| scenario.command.as_deref() == Some(command.as_str()))
        {
            findings.push(finding(
                "homeboy.json",
                command,
                "required command has no declared golden output fixture".to_string(),
                "Add a command_status_contracts scenario with a JSON fixture for this structured-output command.".to_string(),
            ));
        }
    }

    for command in &config.required_output_error_commands {
        if !config.scenarios.iter().any(|scenario| {
            scenario.command.as_deref() == Some(command.as_str())
                && scenario.output_file
                && is_validation_error_scenario(scenario)
        }) {
            findings.push(finding(
                "homeboy.json",
                command,
                "required command lacks a validation-error --output golden fixture".to_string(),
                "Add an output_file validation_error scenario so validation failures prove they still write structured --output JSON.".to_string(),
            ));
        }
    }

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

        findings.extend(validate_json_envelope(scenario, &json));

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

fn is_validation_error_scenario(scenario: &CommandStatusContractScenario) -> bool {
    scenario.outcome.as_deref() == Some("validation_error")
        || scenario.id.contains("validation-error")
        || scenario.id.contains("validation_error")
}

fn validate_json_envelope(
    scenario: &CommandStatusContractScenario,
    json: &serde_json::Value,
) -> Vec<Finding> {
    if let Some(items) = json.get("scenarios").and_then(|value| value.as_array()) {
        let mut findings = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match item.get("payload") {
                Some(payload) => findings.extend(validate_single_json_envelope(
                    scenario,
                    payload,
                    &format!("/scenarios/{index}/payload"),
                )),
                None => findings.push(finding(
                    &scenario.file,
                    &scenario.id,
                    format!("golden contract scenario at /scenarios/{index} is missing /payload"),
                    "Store grouped command contract fixtures as scenarios containing Homeboy CLI envelope payloads.".to_string(),
                )),
            }
        }
        return findings;
    }

    validate_single_json_envelope(scenario, json, "")
}

fn validate_single_json_envelope(
    scenario: &CommandStatusContractScenario,
    json: &serde_json::Value,
    pointer_prefix: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    match json.pointer("/success") {
        Some(serde_json::Value::Bool(true)) => {
            if json.pointer("/data").is_none() {
                findings.push(finding(
                    &scenario.file,
                    &scenario.id,
                    format!("successful JSON envelope is missing {pointer_prefix}/data"),
                    "Store command output fixtures as Homeboy CLI envelopes with success=true and data.".to_string(),
                ));
            }
        }
        Some(serde_json::Value::Bool(false)) => {
            if json.pointer("/data").is_none() && json.pointer("/error").is_none() {
                findings.push(finding(
                    &scenario.file,
                    &scenario.id,
                    format!("failed JSON envelope is missing {pointer_prefix}/data or {pointer_prefix}/error"),
                    "Store non-zero command fixtures as Homeboy CLI envelopes with either command data or error details.".to_string(),
                ));
            }
        }
        Some(_) => findings.push(finding(
            &scenario.file,
            &scenario.id,
            format!("JSON envelope {pointer_prefix}/success is not a boolean"),
            "Use the standard Homeboy CLI JSON envelope shape for command output fixtures."
                .to_string(),
        )),
        None => findings.push(finding(
            &scenario.file,
            &scenario.id,
            format!("JSON envelope is missing {pointer_prefix}/success"),
            "Use the standard Homeboy CLI JSON envelope shape for command output fixtures."
                .to_string(),
        )),
    }

    if is_validation_error_scenario(scenario) {
        if json.pointer("/success") != Some(&serde_json::Value::Bool(false)) {
            findings.push(finding(
                &scenario.file,
                &scenario.id,
                format!("validation-error scenario must have {pointer_prefix}/success=false"),
                "Capture the validation failure envelope instead of a successful command response."
                    .to_string(),
            ));
        }
        if json.pointer("/error/code").is_none() {
            findings.push(finding(
                &scenario.file,
                &scenario.id,
                format!("validation-error scenario is missing {pointer_prefix}/error/code"),
                "Expose the stable validation error code in the command output contract fixture."
                    .to_string(),
            ));
        }
    }

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
            required_commands: Vec::new(),
            required_output_error_commands: Vec::new(),
            scenarios: vec![scenario(
                "refactor-transform-no-match",
                Some("refactor transform"),
                "no-match.json",
                None,
                false,
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
            required_commands: Vec::new(),
            required_output_error_commands: Vec::new(),
            scenarios: vec![scenario(
                "refactor-transform-no-match",
                Some("refactor transform"),
                "no-match.json",
                None,
                false,
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

    #[test]
    fn required_command_without_fixture_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = CommandStatusContractConfig {
            required_commands: vec!["audit".to_string()],
            required_output_error_commands: Vec::new(),
            scenarios: Vec::new(),
        };

        let findings = run(dir.path(), &config);

        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .description
            .contains("required command has no declared golden output fixture"));
    }

    #[test]
    fn required_output_error_command_without_validation_fixture_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("audit.json"),
            r#"{"success":true,"data":{}}"#,
        )
        .expect("fixture");
        let config = CommandStatusContractConfig {
            required_commands: Vec::new(),
            required_output_error_commands: vec!["audit".to_string()],
            scenarios: vec![scenario(
                "audit-success",
                Some("audit"),
                "audit.json",
                None,
                false,
                [],
            )],
        };

        let findings = run(dir.path(), &config);

        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .description
            .contains("validation-error --output golden fixture"));
    }

    #[test]
    fn validation_error_fixture_requires_error_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("error.json"),
            r#"{"success":false,"error":{}}"#,
        )
        .expect("fixture");
        let config = CommandStatusContractConfig {
            required_commands: Vec::new(),
            required_output_error_commands: Vec::new(),
            scenarios: vec![scenario(
                "audit-validation-error",
                Some("audit"),
                "error.json",
                Some("validation_error"),
                true,
                [],
            )],
        };

        let findings = run(dir.path(), &config);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("/error/code"));
    }

    fn scenario<const N: usize>(
        id: &str,
        command: Option<&str>,
        file: &str,
        outcome: Option<&str>,
        output_file: bool,
        expected: [(&str, serde_json::Value); N],
    ) -> CommandStatusContractScenario {
        CommandStatusContractScenario {
            id: id.to_string(),
            command: command.map(str::to_string),
            file: file.to_string(),
            outcome: outcome.map(str::to_string),
            output_file,
            expected_fields: expected
                .into_iter()
                .map(|(pointer, value)| (pointer.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
        }
    }
}
