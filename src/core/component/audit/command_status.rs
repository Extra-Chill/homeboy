use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CommandStatusContractConfig {
    /// Visible command paths that must have at least one golden output fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_commands: Vec<String>,
    /// Visible command paths that must declare a validation-error scenario using
    /// `--output`, proving error responses still write the structured file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_output_error_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<CommandStatusContractScenario>,
}

impl CommandStatusContractConfig {
    pub fn is_empty(&self) -> bool {
        self.required_commands.is_empty()
            && self.required_output_error_commands.is_empty()
            && self.scenarios.is_empty()
    }

    pub(super) fn merge(&mut self, other: &CommandStatusContractConfig) {
        for command in &other.required_commands {
            if !self.required_commands.contains(command) {
                self.required_commands.push(command.clone());
            }
        }
        for command in &other.required_output_error_commands {
            if !self.required_output_error_commands.contains(command) {
                self.required_output_error_commands.push(command.clone());
            }
        }
        for scenario in &other.scenarios {
            if !self
                .scenarios
                .iter()
                .any(|existing| existing.id == scenario.id)
            {
                self.scenarios.push(scenario.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandStatusContractScenario {
    /// Stable scenario id shown in findings.
    pub id: String,
    /// Visible command path this fixture covers, e.g. `audit` or `runs list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// JSON fixture path relative to the component root.
    pub file: String,
    /// Scenario outcome class. `validation_error` requires a failed envelope
    /// with an error object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Whether this scenario is expected to cover the global `--output` file path.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_file: bool,
    /// Expected JSON Pointer fields and values, e.g. `/success: true`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expected_fields: BTreeMap<String, serde_json::Value>,
    /// Expected status label for declared status fields, e.g. `planned`,
    /// `skipped`, `empty`, `failed`, or `completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<String>,
    /// JSON Pointer fields that must equal `expected_status`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_fields: Vec<String>,
    /// Expected dry-run value for declared dry-run fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_dry_run: Option<bool>,
    /// JSON Pointer fields that must equal `expected_dry_run`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dry_run_fields: Vec<String>,
    /// Expected top-level Homeboy envelope success value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_success: Option<bool>,
    /// This scenario intentionally represents empty/no-op work that should
    /// succeed rather than report an error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub empty_success: bool,
}
