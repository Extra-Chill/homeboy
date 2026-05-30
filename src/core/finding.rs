//! Shared typed finding contract for Homeboy producers.
//!
//! `HomeboyFinding` is the command/extension/reporting shape. Observation
//! storage records are a projection of this model, not the generic contract
//! producers should build first.

use serde::{Deserialize, Serialize};

/// A normalized finding emitted by a Homeboy command, extension, or report step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyFinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "HomeboyFindingLocation::is_empty")]
    pub location: HomeboyFindingLocation,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixable: Option<bool>,
    #[serde(default, skip_serializing_if = "HomeboyFindingProducer::is_empty")]
    pub producer: HomeboyFindingProducer,
    #[serde(default, skip_serializing_if = "is_empty_json_object")]
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl HomeboyFinding {
    pub fn builder(tool: impl Into<String>, message: impl Into<String>) -> HomeboyFindingBuilder {
        HomeboyFindingBuilder::new(tool, message)
    }
}

/// Optional source location for a finding.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyFindingLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
}

impl HomeboyFindingLocation {
    pub fn is_empty(&self) -> bool {
        self.file.is_none() && self.line.is_none() && self.column.is_none()
    }
}

/// Provenance for the producer that created a finding.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyFindingProducer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sidecar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_artifact: Option<String>,
}

impl HomeboyFindingProducer {
    pub fn is_empty(&self) -> bool {
        self.command.is_none()
            && self.extension.is_none()
            && self.step.is_none()
            && self.source_sidecar.is_none()
            && self.source_artifact.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct HomeboyFindingBuilder {
    finding: HomeboyFinding,
}

impl HomeboyFindingBuilder {
    pub fn new(tool: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            finding: HomeboyFinding {
                id: None,
                tool: tool.into(),
                rule: None,
                category: None,
                severity: None,
                fingerprint: None,
                location: HomeboyFindingLocation::default(),
                message: message.into(),
                fixable: None,
                producer: HomeboyFindingProducer::default(),
                metadata: serde_json::json!({}),
                raw: None,
            },
        }
    }

    pub fn id(mut self, value: impl Into<String>) -> Self {
        self.finding.id = Some(value.into());
        self
    }

    pub fn rule(mut self, value: impl Into<String>) -> Self {
        self.finding.rule = Some(value.into());
        self
    }

    pub fn category(mut self, value: impl Into<String>) -> Self {
        self.finding.category = Some(value.into());
        self
    }

    pub fn severity(mut self, value: impl Into<String>) -> Self {
        self.finding.severity = Some(value.into());
        self
    }

    pub fn fingerprint(mut self, value: impl Into<String>) -> Self {
        self.finding.fingerprint = Some(value.into());
        self
    }

    pub fn file(mut self, value: impl Into<String>) -> Self {
        self.finding.location.file = Some(value.into());
        self
    }

    pub fn line(mut self, value: impl Into<i64>) -> Self {
        self.finding.location.line = Some(value.into());
        self
    }

    pub fn column(mut self, value: impl Into<i64>) -> Self {
        self.finding.location.column = Some(value.into());
        self
    }

    pub fn fixable(mut self, value: bool) -> Self {
        self.finding.fixable = Some(value);
        self
    }

    pub fn producer(mut self, producer: HomeboyFindingProducer) -> Self {
        self.finding.producer = producer;
        self
    }

    pub fn command(mut self, value: impl Into<String>) -> Self {
        self.finding.producer.command = Some(value.into());
        self
    }

    pub fn extension(mut self, value: impl Into<String>) -> Self {
        self.finding.producer.extension = Some(value.into());
        self
    }

    pub fn step(mut self, value: impl Into<String>) -> Self {
        self.finding.producer.step = Some(value.into());
        self
    }

    pub fn source_sidecar(mut self, value: impl Into<String>) -> Self {
        self.finding.producer.source_sidecar = Some(value.into());
        self
    }

    pub fn source_artifact(mut self, value: impl Into<String>) -> Self {
        self.finding.producer.source_artifact = Some(value.into());
        self
    }

    pub fn metadata(mut self, value: serde_json::Value) -> Self {
        self.finding.metadata = value;
        self
    }

    pub fn raw<T: Serialize>(mut self, value: T) -> Self {
        self.finding.raw = Some(serde_json::to_value(value).unwrap_or(serde_json::Value::Null));
        self
    }

    pub fn build(self) -> HomeboyFinding {
        self.finding
    }
}

fn is_empty_json_object(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn represents_lint_finding_data() {
        let finding = HomeboyFinding::builder("phpcs", "escape output")
            .rule("WordPress.Security.EscapeOutput")
            .category("security")
            .severity("error")
            .fingerprint("src/foo.php:12:WordPress.Security.EscapeOutput")
            .file("src/foo.php")
            .line(12)
            .fixable(true)
            .command("homeboy lint")
            .source_sidecar("lint-findings")
            .raw(serde_json::json!({ "sniff": "WordPress.Security.EscapeOutput" }))
            .build();

        assert_eq!(finding.tool, "phpcs");
        assert_eq!(finding.category.as_deref(), Some("security"));
        assert_eq!(finding.fixable, Some(true));
        assert_eq!(
            finding.producer.source_sidecar.as_deref(),
            Some("lint-findings")
        );
    }

    #[test]
    fn represents_audit_finding_data() {
        let finding = HomeboyFinding::builder("audit", "unused parameter")
            .rule("unused_parameter")
            .category("code_audit")
            .severity("warning")
            .file("src/core/foo.rs")
            .fingerprint("src/core/foo.rs:unused_parameter")
            .metadata(serde_json::json!({
                "convention": "parameter use",
                "confidence": "graph",
                "suggestion": "Remove the parameter or wire it into behavior"
            }))
            .build();

        assert_eq!(finding.rule.as_deref(), Some("unused_parameter"));
        assert_eq!(finding.metadata["confidence"], "graph");
    }

    #[test]
    fn represents_test_failure_data() {
        let finding = HomeboyFinding::builder("test", "ExampleTest::test_save failed")
            .rule("assertion_mismatch")
            .category("test_failure")
            .severity("error")
            .file("tests/example_test.rs")
            .line(44)
            .fingerprint("tests/example_test.rs:ExampleTest::test_save")
            .metadata(serde_json::json!({
                "test_name": "ExampleTest::test_save",
                "error_type": "AssertionFailed",
                "cluster": "assertion_mismatch"
            }))
            .build();

        assert_eq!(finding.tool, "test");
        assert_eq!(finding.category.as_deref(), Some("test_failure"));
        assert_eq!(finding.location.line, Some(44));
        assert_eq!(finding.metadata["test_name"], "ExampleTest::test_save");
    }

    #[test]
    fn represents_bench_budget_data() {
        let finding = HomeboyFinding::builder("budget", "Page ready time exceeded budget")
            .rule("page.ready_ms")
            .category("budget")
            .severity("error")
            .fingerprint("page.ready_ms:front-page")
            .metadata(serde_json::json!({
                "actual": 1200.0,
                "expected": 1000.0,
                "unit": "ms",
                "subject": "front-page"
            }))
            .build();

        assert_eq!(finding.tool, "budget");
        assert_eq!(finding.metadata["unit"], "ms");
    }

    #[test]
    fn represents_annotation_data() {
        let finding = HomeboyFinding::builder("compiler", "unused variable")
            .rule("unused_variables")
            .category("annotation")
            .severity("warning")
            .file("src/main.rs")
            .line(8)
            .column(13)
            .source_sidecar("annotations")
            .source_artifact("compiler-warnings.json")
            .build();

        assert_eq!(finding.location.column, Some(13));
        assert_eq!(
            finding.producer.source_artifact.as_deref(),
            Some("compiler-warnings.json")
        );
    }
}
