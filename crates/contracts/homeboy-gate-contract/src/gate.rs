use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plan::HomeboyPlan;

pub const HOMEBOY_GATE_RESULT_SCHEMA: &str = "homeboy/gate-result/v1";
pub const EXTERNAL_CHECK_PUBLICATION_SCHEMA: &str = "homeboy/external-check-publication/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalCheckPublication {
    pub schema: String,
    pub provider: String,
    pub repository: String,
    pub base_sha: String,
    pub head_sha: String,
    pub gate_id: String,
    pub check_id: String,
    pub environment_digest: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub observed_at: String,
    pub sequence: u64,
    pub evidence_id: String,
    pub authoritative: bool,
    pub hydrated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalCheckOutcome {
    Pending,
    Succeeded,
    Failed,
}

impl ExternalCheckPublication {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != EXTERNAL_CHECK_PUBLICATION_SCHEMA {
            return Err("unsupported external check publication schema".to_string());
        }
        if !self.authoritative || !self.hydrated {
            return Err("external check publication is not authoritative and hydrated".to_string());
        }
        for (name, value) in [
            ("provider", &self.provider),
            ("repository", &self.repository),
            ("base_sha", &self.base_sha),
            ("head_sha", &self.head_sha),
            ("gate_id", &self.gate_id),
            ("check_id", &self.check_id),
            ("environment_digest", &self.environment_digest),
            ("observed_at", &self.observed_at),
            ("evidence_id", &self.evidence_id),
        ] {
            if value.is_empty() {
                return Err(format!("external check publication {name} is empty"));
            }
        }
        if !matches!(
            self.status.as_str(),
            "queued"
                | "in_progress"
                | "waiting"
                | "pending"
                | "completed"
                | "success"
                | "failure"
                | "cancelled"
                | "timed_out"
                | "action_required"
                | "neutral"
                | "skipped"
        ) {
            return Err(format!(
                "unsupported external check status `{}`",
                self.status
            ));
        }
        Ok(())
    }

    pub fn outcome(&self) -> ExternalCheckOutcome {
        match self.status.as_str() {
            "success" => match self.conclusion.as_deref() {
                None | Some("success") => ExternalCheckOutcome::Succeeded,
                Some(_) => ExternalCheckOutcome::Failed,
            },
            "completed" => match self.conclusion.as_deref() {
                Some("success") => ExternalCheckOutcome::Succeeded,
                Some(
                    "failure" | "cancelled" | "timed_out" | "action_required" | "neutral"
                    | "skipped",
                ) => ExternalCheckOutcome::Failed,
                _ => ExternalCheckOutcome::Pending,
            },
            "failure" | "cancelled" | "timed_out" | "action_required" | "neutral" | "skipped" => {
                ExternalCheckOutcome::Failed
            }
            _ => ExternalCheckOutcome::Pending,
        }
    }
}

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
    /// The candidate failed, but the same failure was reproduced against its
    /// immutable baseline. This remains red unless a caller explicitly accepts
    /// the proven non-regression for its own policy boundary.
    AcceptedInheritedFailure,
    Skipped,
    Blocked,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyGateVisibility {
    #[default]
    Visible,
    Private,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
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

    fn publication(status: &str, conclusion: Option<&str>) -> ExternalCheckPublication {
        ExternalCheckPublication {
            schema: EXTERNAL_CHECK_PUBLICATION_SCHEMA.into(),
            provider: "github".into(),
            repository: "Extra-Chill/homeboy".into(),
            base_sha: "base".into(),
            head_sha: "head".into(),
            gate_id: "ci".into(),
            check_id: "test".into(),
            environment_digest: "sha256:env".into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            observed_at: "2026-09-20T19:00:00Z".into(),
            sequence: 1,
            evidence_id: "evidence-1".into(),
            authoritative: true,
            hydrated: true,
        }
    }

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
        let plan = crate::plan::HomeboyPlan::builder_for_component(
            crate::plan::PlanKind::Quality,
            "fixture",
        )
        .steps(vec![crate::plan::PlanStep::ready("verify", "gate.command")
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

    #[test]
    fn external_publication_requires_hydration_and_conclusion_success() {
        let mut pending = publication("completed", None);
        pending.validate().unwrap();
        assert_eq!(pending.outcome(), ExternalCheckOutcome::Pending);
        pending.conclusion = Some("failure".into());
        assert_eq!(pending.outcome(), ExternalCheckOutcome::Failed);
        pending.conclusion = Some("success".into());
        assert_eq!(pending.outcome(), ExternalCheckOutcome::Succeeded);
        pending.status = "success".into();
        pending.conclusion = Some("failure".into());
        assert_eq!(pending.outcome(), ExternalCheckOutcome::Failed);
        pending.hydrated = false;
        assert!(pending.validate().is_err());
    }

    #[test]
    fn external_publication_rejects_unknown_status() {
        let invalid = publication("bogus", None);
        assert!(invalid.validate().is_err());
    }
}
