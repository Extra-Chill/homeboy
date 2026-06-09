use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::plan::HomeboyPlan;

pub const HOMEBOY_GATE_RESULT_SCHEMA: &str = "homeboy/gate-result/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeboyGateResult {
    #[serde(default = "gate_result_schema")]
    pub schema: String,
    pub id: String,
    pub name: String,
    pub kind: HomeboyGateKind,
    pub status: HomeboyGateStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub evidence: Value,
    #[serde(default)]
    pub visibility: HomeboyGateVisibility,
    #[serde(default)]
    pub reveal_policy: HomeboyGateRevealPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_feedback: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provenance: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyGateKind {
    Command,
    Metric,
    Capability,
    Approval,
    Policy,
    Quality,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyGateStatus {
    Passed,
    Failed,
    Skipped,
    Blocked,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyGateVisibility {
    #[default]
    Visible,
    Private,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyGateRevealPolicy {
    #[default]
    FullEvidence,
    SummaryOnly,
    Redacted,
    NoDetail,
}

impl HomeboyGateResult {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: HomeboyGateKind,
        status: HomeboyGateStatus,
    ) -> Self {
        Self {
            schema: HOMEBOY_GATE_RESULT_SCHEMA.to_string(),
            id: id.into(),
            name: name.into(),
            kind,
            status,
            summary: String::new(),
            detail: None,
            evidence: Value::Null,
            visibility: HomeboyGateVisibility::Visible,
            reveal_policy: HomeboyGateRevealPolicy::FullEvidence,
            retryable: None,
            agent_feedback: String::new(),
            provenance: Value::Null,
        }
    }

    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn evidence(mut self, evidence: Value) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn visibility(mut self, visibility: HomeboyGateVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn reveal_policy(mut self, reveal_policy: HomeboyGateRevealPolicy) -> Self {
        self.reveal_policy = reveal_policy;
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = Some(retryable);
        self
    }

    pub fn agent_feedback(mut self, agent_feedback: impl Into<String>) -> Self {
        self.agent_feedback = agent_feedback.into();
        self
    }

    pub fn provenance(mut self, provenance: Value) -> Self {
        self.provenance = provenance;
        self
    }
}

pub fn collect_plan_gate_results(plan: &HomeboyPlan) -> Vec<HomeboyGateResult> {
    plan.steps
        .iter()
        .filter_map(|step| step.outputs.get("gate_result"))
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}

fn gate_result_schema() -> String {
    HOMEBOY_GATE_RESULT_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_result_serializes_with_stable_schema() {
        let result = HomeboyGateResult::new(
            "gate-1",
            "cargo test",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        )
        .summary("targeted tests passed")
        .retryable(false);

        let value = serde_json::to_value(result).expect("serialize gate result");

        assert_eq!(value["schema"], HOMEBOY_GATE_RESULT_SCHEMA);
        assert_eq!(value["id"], "gate-1");
        assert_eq!(value["kind"], "command");
        assert_eq!(value["status"], "passed");
        assert_eq!(value["retryable"], false);
    }

    #[test]
    fn collect_plan_gate_results_reads_step_outputs() {
        let plan = crate::core::plan::HomeboyPlan::builder_for_component(
            crate::core::plan::PlanKind::Quality,
            "fixture",
        )
        .steps(vec![crate::core::plan::PlanStep::ready(
            "verify",
            "gate.command",
        )
        .gate_result(HomeboyGateResult::new(
            "gate-1",
            "cargo test",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        ))
        .build()])
        .build();

        let results = collect_plan_gate_results(&plan);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "gate-1");
        assert_eq!(results[0].status, HomeboyGateStatus::Passed);
    }
}
