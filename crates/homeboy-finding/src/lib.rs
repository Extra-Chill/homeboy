//! Shared typed finding substrate for Homeboy workflows.
//!
//! Domain-specific commands can keep their detailed sidecar contracts while
//! adapting toward this shape before persistence, reporting, or issue grouping.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A normalized finding emitted by lint, audit, test, bench, review, or custom
/// extension producers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyFinding {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(flatten)]
    pub location: FindingLocation,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(flatten)]
    pub fix: FindingFix,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<FindingProducer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FindingSource>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

impl HomeboyFinding {
    pub fn builder(tool: impl Into<String>, message: impl Into<String>) -> HomeboyFindingBuilder {
        HomeboyFindingBuilder::new(tool, message)
    }

    pub fn metadata_json(&self) -> Value {
        let mut metadata = self.metadata.clone();
        insert_option(&mut metadata, "category", self.category.clone());
        insert_option(&mut metadata, "column", self.location.column);
        insert_option_value(&mut metadata, "producer", &self.producer);
        insert_option_value(&mut metadata, "source", &self.source);
        if let Some(raw) = &self.raw {
            metadata.insert("raw".to_string(), raw.clone());
        }
        Value::Object(metadata)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingFix {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingProducer {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingProducerSummary {
    pub tool: String,
    pub status: String,
    pub finding_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FindingSource>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

impl FindingProducerSummary {
    pub fn new(tool: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            status: status.into(),
            finding_count: 0,
            step: None,
            source: None,
            metadata: Map::new(),
        }
    }

    pub fn finding_count(mut self, finding_count: usize) -> Self {
        self.finding_count = finding_count;
        self
    }

    pub fn step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    pub fn source(mut self, source: FindingSource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn metadata<T: Serialize>(mut self, key: impl Into<String>, value: T) -> Self {
        self.metadata.insert(
            key.into(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
        self
    }
}

impl FindingProducer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            invocation: None,
        }
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn invocation(mut self, invocation: impl Into<String>) -> Self {
        self.invocation = Some(invocation.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl FindingSource {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            label: None,
            path: None,
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct HomeboyFindingBuilder {
    finding: HomeboyFinding,
}

impl HomeboyFindingBuilder {
    fn new(tool: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            finding: HomeboyFinding {
                tool: tool.into(),
                rule: None,
                category: None,
                severity: None,
                location: FindingLocation::default(),
                message: message.into(),
                fingerprint: None,
                fix: FindingFix::default(),
                producer: None,
                source: None,
                metadata: Map::new(),
                raw: None,
            },
        }
    }

    pub fn rule(mut self, rule: impl Into<String>) -> Self {
        self.finding.rule = Some(rule.into());
        self
    }

    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.finding.category = Some(category.into());
        self
    }

    pub fn severity(mut self, severity: impl Into<String>) -> Self {
        self.finding.severity = Some(severity.into());
        self
    }

    pub fn file(mut self, file: impl Into<String>) -> Self {
        self.finding.location.file = Some(file.into());
        self
    }

    pub fn line(mut self, line: impl Into<i64>) -> Self {
        self.finding.location.line = Some(line.into());
        self
    }

    pub fn column(mut self, column: impl Into<i64>) -> Self {
        self.finding.location.column = Some(column.into());
        self
    }

    pub fn fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.finding.fingerprint = Some(fingerprint.into());
        self
    }

    pub fn fixable(mut self, fixable: bool) -> Self {
        self.finding.fix.fixable = Some(fixable);
        self
    }

    pub fn producer(mut self, producer: FindingProducer) -> Self {
        self.finding.producer = Some(producer);
        self
    }

    pub fn source(mut self, source: FindingSource) -> Self {
        self.finding.source = Some(source);
        self
    }

    pub fn metadata<T: Serialize>(mut self, key: impl Into<String>, value: T) -> Self {
        self.finding.metadata.insert(
            key.into(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
        self
    }

    pub fn raw<T: Serialize>(mut self, raw: T) -> Self {
        self.finding.raw = Some(serde_json::to_value(raw).unwrap_or(serde_json::Value::Null));
        self
    }

    pub fn build(self) -> HomeboyFinding {
        self.finding
    }
}

fn insert_option<T: Serialize>(metadata: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        metadata.insert(
            key.to_string(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
    }
}

fn insert_option_value<T: Serialize>(
    metadata: &mut Map<String, Value>,
    key: &str,
    value: &Option<T>,
) {
    if let Some(value) = value {
        metadata.insert(
            key.to_string(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_covers_lint_shape() {
        let finding = HomeboyFinding::builder("phpcs", "escape output")
            .rule("WordPress.Security.EscapeOutput")
            .category("security")
            .severity("error")
            .file("src/lib.php")
            .line(12)
            .column(8)
            .fingerprint("src/lib.php:12:8:WordPress.Security.EscapeOutput")
            .fixable(true)
            .source(FindingSource::new("sidecar").path("lint-findings.json"))
            .build();

        assert_eq!(finding.category.as_deref(), Some("security"));
        assert_eq!(finding.location.column, Some(8));
        assert_eq!(
            finding.metadata_json()["source"]["path"],
            "lint-findings.json"
        );
    }

    #[test]
    fn builder_covers_audit_shape() {
        let finding = HomeboyFinding::builder("audit", "Missing run function")
            .rule("missing_method")
            .category("command modules")
            .severity("warning")
            .file("src/commands/foo.rs")
            .metadata("confidence", "structural")
            .metadata("suggestion", "Add run()")
            .build();

        assert_eq!(finding.rule.as_deref(), Some("missing_method"));
        assert_eq!(finding.metadata_json()["suggestion"], "Add run()");
    }

    #[test]
    fn builder_covers_test_failure_shape() {
        let finding = HomeboyFinding::builder("test", "Expected 200, got 500")
            .rule("assertion_mismatch")
            .category("test_failure")
            .severity("error")
            .file("tests/AuthTest.php")
            .line(44)
            .fingerprint("AuthTest::test_login:assertion_mismatch")
            .metadata("test_name", "AuthTest::test_login")
            .source(FindingSource::new("runner").label("phpunit"))
            .build();

        assert_eq!(finding.tool, "test");
        assert_eq!(finding.metadata_json()["test_name"], "AuthTest::test_login");
    }

    #[test]
    fn builder_covers_bench_budget_shape() {
        let finding = HomeboyFinding::builder("budget", "Page ready time exceeded budget")
            .rule("page.ready_ms")
            .category("budget")
            .severity("error")
            .fingerprint("page.ready_ms:front-page")
            .metadata("actual", 1200.0)
            .metadata("expected", 1000.0)
            .metadata("unit", "ms")
            .build();

        assert_eq!(finding.rule.as_deref(), Some("page.ready_ms"));
        assert_eq!(finding.metadata_json()["unit"], "ms");
    }

    #[test]
    fn builder_covers_annotation_shape() {
        let finding = HomeboyFinding::builder("github-annotations", "escape output")
            .rule("WordPress.Security.EscapeOutput")
            .severity("warning")
            .file("src/lib.php")
            .line(12)
            .source(FindingSource::new("annotation").path("phpcs.json"))
            .metadata("github_level", "warning")
            .build();

        assert_eq!(finding.source.as_ref().unwrap().kind, "annotation");
        assert_eq!(finding.metadata_json()["github_level"], "warning");
    }
}
