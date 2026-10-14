//! Extension-declared workload kinds (#15840).
//!
//! An extension declares each workload kind it runs under the top-level
//! `workloads` manifest map, keyed by kind:
//!
//! ```json
//! "workloads": {
//!   "fuzz": {
//!     "script": "scripts/workload/fuzz.sh",
//!     "workloads": [{ "id": "parser" }],
//!     "list_probes": [{ "type": "json_pointer", "path": "package.json", "pointer": "/scripts/fuzz", "id": "package-script" }],
//!     "replay_command": "bash scripts/workload/fuzz-replay.sh",
//!     "compare": {
//!       "metrics": [{ "name": "cases_failed", "direction": "lower_is_better" }],
//!       "findings_key": "fingerprint"
//!     },
//!     "safety_class": "read_only"
//!   }
//! }
//! ```
//!
//! Core never enumerates kinds. It runs whatever is declared, reads the
//! generic result envelope ([`crate::workload_result`]) and compares runs by
//! the declared metrics and the findings key. Everything kind-specific stays
//! in the extension and in the envelope's opaque `payload`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::runtime_helper::RuntimeHelperRequirement;

/// Prefix of the Extension API capability id a declared kind is published
/// under: kind `fuzz` is the capability `workload.fuzz`.
pub const WORKLOAD_CAPABILITY_PREFIX: &str = "workload.";

/// The Extension API capability id of a workload kind.
pub fn workload_capability_id(kind: &str) -> String {
    format!("{WORKLOAD_CAPABILITY_PREFIX}{kind}")
}

/// One extension-declared workload kind.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadKindConfig {
    /// Runner script, relative to the extension root. Required to run; a kind
    /// without one still lists its declared workloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Environment variable names the runner reads, beyond the generic
    /// `HOMEBOY_WORKLOAD_*` contract. Declarative documentation for operators
    /// and contract checks; core does not forward undeclared host variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Core-owned helper capabilities the runner requires.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_helpers: Vec<RuntimeHelperRequirement>,
    /// Statically declared workloads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<WorkloadDeclaration>,
    /// Discovery probes that declare workloads from the component.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list_probes: Vec<WorkloadListProbe>,
    /// Schema id the envelope's `payload.schema` must carry. Undeclared means
    /// the payload is fully opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<String>,
    /// Shell command that replays one case. Receives the case env
    /// (`HOMEBOY_WORKLOAD_CASE_ID`, `_SEED`, `_ARTIFACT_REF`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_command: Option<String>,
    /// Shell command that minimizes one case, with the same env as replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimize_command: Option<String>,
    /// Whether the kind supports campaign/resume orchestration.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub campaign: bool,
    /// How two runs of this kind compare and gate.
    #[serde(default, skip_serializing_if = "WorkloadCompareConfig::is_default")]
    pub compare: WorkloadCompareConfig,
    /// Whether running a workload can mutate the environment it runs in.
    #[serde(default, skip_serializing_if = "WorkloadSafetyClass::is_read_only")]
    pub safety_class: WorkloadSafetyClass,
}

/// A statically declared workload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadDeclaration {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// How workloads are discovered from a component.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkloadListProbe {
    /// The runner lists its own workloads: Homeboy runs the script with
    /// `HOMEBOY_WORKLOAD_LIST_ONLY=1` and reads
    /// `{"workloads": [{"id", "label"?, "description"?}]}` from
    /// `HOMEBOY_WORKLOAD_RESULTS_FILE`.
    Script,
    /// A JSON file in the component declares a workload when the RFC 6901
    /// pointer resolves to a non-empty string.
    JsonPointer {
        /// JSON file relative to the component root.
        path: String,
        pointer: String,
        /// Workload id emitted when the pointer resolves.
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

/// Declared compare/gate semantics for a kind.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadCompareConfig {
    /// Metrics the gate reads, with their direction and noise threshold.
    /// Envelope metrics not declared here are reported but never gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<WorkloadMetricPolicy>,
    /// How findings are matched across two runs.
    #[serde(default)]
    pub findings_key: WorkloadFindingsKey,
}

impl WorkloadCompareConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Direction and threshold of one gated metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadMetricPolicy {
    pub name: String,
    pub direction: WorkloadMetricDirection,
    /// Change, in percent of the baseline value, tolerated before the metric
    /// counts as regressed or improved. Absent means any change counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadMetricDirection {
    LowerIsBetter,
    HigherIsBetter,
}

/// The identity findings are matched by across runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadFindingsKey {
    /// `HomeboyFinding.fingerprint`. A finding without one is matched by
    /// `tool|rule|message`.
    #[default]
    Fingerprint,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadSafetyClass {
    /// Running cannot mutate anything outside the run directory.
    #[default]
    ReadOnly,
    /// Running may mutate its environment. Requires `--allow-destructive`
    /// with an explicit `homeboy/isolation-proof/v1`.
    Destructive,
}

impl WorkloadSafetyClass {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Destructive => "destructive",
        }
    }
}

/// A kind id is a lowercase slug: it names a run kind, an Extension API
/// capability (`workload.<kind>`) and a baseline slot.
pub fn is_valid_workload_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && kind.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn invalid(
    field: impl Into<String>,
    problem: impl Into<String>,
    value: &str,
) -> homeboy_error::Error {
    homeboy_error::Error::validation_invalid_argument(
        field.into(),
        problem.into(),
        Some(value.to_string()),
        None,
    )
}

impl WorkloadKindConfig {
    /// Reject declarations that would be silently inert or ambiguous.
    pub fn validate(&self, kind: &str) -> homeboy_error::Result<()> {
        let field = |name: &str| format!("workloads.{kind}.{name}");
        if !is_valid_workload_kind(kind) {
            return Err(invalid(
                "workloads",
                "workload kind must be a lowercase slug of [a-z0-9_-] starting with a letter",
                kind,
            ));
        }
        for (name, value) in [
            ("script", self.script.as_deref()),
            ("replay_command", self.replay_command.as_deref()),
            ("minimize_command", self.minimize_command.as_deref()),
            ("result_schema", self.result_schema.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(invalid(field(name), "must not be empty when declared", ""));
            }
        }
        let mut ids = BTreeSet::new();
        for workload in &self.workloads {
            if workload.id.trim().is_empty() {
                return Err(invalid(field("workloads.id"), "must not be empty", ""));
            }
            if !ids.insert(workload.id.as_str()) {
                return Err(invalid(
                    field("workloads.id"),
                    "must be unique within a kind",
                    &workload.id,
                ));
            }
        }
        for probe in &self.list_probes {
            if let WorkloadListProbe::JsonPointer {
                path, pointer, id, ..
            } = probe
            {
                if path.trim().is_empty() || id.trim().is_empty() {
                    return Err(invalid(
                        field("list_probes"),
                        "json_pointer probes need a path and an id",
                        id,
                    ));
                }
                if !pointer.is_empty() && !pointer.starts_with('/') {
                    return Err(invalid(
                        field("list_probes.pointer"),
                        "must be an RFC 6901 JSON pointer",
                        pointer,
                    ));
                }
            }
        }
        let mut metrics = BTreeSet::new();
        for metric in &self.compare.metrics {
            if metric.name.trim().is_empty() {
                return Err(invalid(
                    field("compare.metrics.name"),
                    "must not be empty",
                    "",
                ));
            }
            if !metrics.insert(metric.name.as_str()) {
                return Err(invalid(
                    field("compare.metrics.name"),
                    "must be unique within a kind",
                    &metric.name,
                ));
            }
            if metric
                .regression_threshold
                .is_some_and(|threshold| !threshold.is_finite() || threshold < 0.0)
            {
                return Err(invalid(
                    field("compare.metrics.regression_threshold"),
                    "must be a finite, non-negative percentage",
                    &metric.name,
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> WorkloadKindConfig {
        serde_json::from_value(value).expect("workload kind config")
    }

    #[test]
    fn full_declaration_round_trips() {
        let value = json!({
            "script": "scripts/workload/fuzz.sh",
            "env": ["NODE_OPTIONS"],
            "runtime_helpers": [{ "id": "runtime-settings" }],
            "workloads": [{ "id": "parser", "label": "Parser" }],
            "list_probes": [
                { "type": "script" },
                { "type": "json_pointer", "path": "package.json", "pointer": "/scripts/fuzz", "id": "package-script" }
            ],
            "result_schema": "homeboy/fuzz-campaign/v1",
            "replay_command": "bash replay.sh",
            "minimize_command": "bash minimize.sh",
            "campaign": true,
            "compare": {
                "metrics": [
                    { "name": "cases_failed", "direction": "lower_is_better" },
                    { "name": "coverage", "direction": "higher_is_better", "regression_threshold": 2.5 }
                ],
                "findings_key": "fingerprint"
            },
            "safety_class": "destructive"
        });
        let config = parse(value.clone());

        assert_eq!(config.script.as_deref(), Some("scripts/workload/fuzz.sh"));
        assert_eq!(config.list_probes[0], WorkloadListProbe::Script);
        assert!(matches!(
            &config.list_probes[1],
            WorkloadListProbe::JsonPointer { pointer, .. } if pointer == "/scripts/fuzz"
        ));
        assert_eq!(
            config.compare.metrics[1].direction,
            WorkloadMetricDirection::HigherIsBetter
        );
        assert_eq!(config.compare.metrics[1].regression_threshold, Some(2.5));
        assert_eq!(config.safety_class, WorkloadSafetyClass::Destructive);
        assert!(config.campaign);
        config.validate("fuzz").expect("valid");
        assert_eq!(serde_json::to_value(&config).unwrap(), value);
    }

    #[test]
    fn minimal_declaration_defaults_to_read_only_fingerprint_matching() {
        let config = parse(json!({ "script": "run.sh" }));

        assert_eq!(config.safety_class, WorkloadSafetyClass::ReadOnly);
        assert_eq!(
            config.compare.findings_key,
            WorkloadFindingsKey::Fingerprint
        );
        assert!(config.compare.metrics.is_empty());
        assert!(!config.campaign);
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            json!({ "script": "run.sh" })
        );
    }

    #[test]
    fn unknown_keys_are_rejected_instead_of_going_inert() {
        let error = serde_json::from_value::<WorkloadKindConfig>(json!({
            "script": "run.sh",
            "replay": "typo"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field `replay`"));

        let error = serde_json::from_value::<WorkloadKindConfig>(json!({
            "compare": { "metrics": [{ "name": "m", "direction": "sideways" }] }
        }))
        .unwrap_err();
        assert!(error.to_string().contains("sideways"));
    }

    #[test]
    fn validation_rejects_bad_kind_ids_duplicates_and_thresholds() {
        let ok = parse(json!({ "script": "run.sh" }));
        assert!(ok.validate("fuzz").is_ok());
        assert!(ok.validate("db-perf_2").is_ok());
        for kind in ["", "Fuzz", "1fuzz", "fuzz.run", "fu zz"] {
            assert!(
                ok.validate(kind).is_err(),
                "kind {kind:?} should be refused"
            );
        }

        let duplicate_workloads = parse(json!({ "workloads": [{ "id": "a" }, { "id": "a" }] }));
        assert!(duplicate_workloads.validate("fuzz").is_err());

        let duplicate_metrics = parse(json!({ "compare": { "metrics": [
            { "name": "m", "direction": "lower_is_better" },
            { "name": "m", "direction": "higher_is_better" }
        ] } }));
        assert!(duplicate_metrics.validate("bench").is_err());

        let negative_threshold = parse(json!({ "compare": { "metrics": [
            { "name": "m", "direction": "lower_is_better", "regression_threshold": -1.0 }
        ] } }));
        assert!(negative_threshold.validate("bench").is_err());

        let empty_replay = parse(json!({ "replay_command": "  " }));
        assert!(empty_replay.validate("fuzz").is_err());

        let bad_pointer = parse(json!({ "list_probes": [
            { "type": "json_pointer", "path": "package.json", "pointer": "scripts/fuzz", "id": "x" }
        ] }));
        assert!(bad_pointer.validate("fuzz").is_err());
    }

    #[test]
    fn capability_id_is_prefixed_kind() {
        assert_eq!(workload_capability_id("fuzz"), "workload.fuzz");
    }
}
