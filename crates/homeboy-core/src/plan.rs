//! Generic typed plan substrate for Homeboy workflows.
//!
//! Domain-specific planners can keep their existing public JSON contracts while
//! adapting toward this shared shape. The common model answers what will run,
//! why it will run, whether it blocks progress, and what artifacts/results are
//! expected.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;

use crate::gate::HomeboyGateResult;
use crate::proof::HomeboyProof;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanValues {
    values: HashMap<String, serde_json::Value>,
}

impl PlanValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn string(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.values
            .insert(key.into(), serde_json::Value::String(value.into()));
        self
    }

    pub fn bool(mut self, key: impl Into<String>, value: bool) -> Self {
        self.values
            .insert(key.into(), serde_json::Value::Bool(value));
        self
    }

    pub fn number(mut self, key: impl Into<String>, value: impl Into<serde_json::Number>) -> Self {
        self.values
            .insert(key.into(), serde_json::Value::Number(value.into()));
        self
    }

    pub fn json<T: Serialize>(mut self, key: impl Into<String>, value: T) -> Self {
        self.values.insert(
            key.into(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
        self
    }
}

impl IntoIterator for PlanValues {
    type Item = (String, serde_json::Value);
    type IntoIter = std::collections::hash_map::IntoIter<String, serde_json::Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}

/// A typed plan that can describe quality, build, release, deploy, PR, CI,
/// refactor, review, or domain-specific workflows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeboyPlan {
    pub id: String,
    pub kind: PlanKind,
    pub subject: PlanSubject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub policy: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<PlanArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<HomeboyProof>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<PlanSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl HomeboyPlan {
    pub fn for_component(kind: PlanKind, component_id: impl Into<String>) -> Self {
        let component_id = component_id.into();
        Self {
            id: format!("{}.{component_id}", kind.slug()),
            kind,
            subject: PlanSubject {
                component_id: Some(component_id),
                ..PlanSubject::default()
            },
            mode: None,
            inputs: HashMap::new(),
            policy: HashMap::new(),
            steps: Vec::new(),
            artifacts: Vec::new(),
            proof: None,
            summary: None,
            warnings: Vec::new(),
            hints: Vec::new(),
        }
    }

    pub fn for_description(kind: PlanKind, description: impl Into<String>) -> Self {
        let description = description.into();
        Self {
            id: format!("{}.{}", kind.slug(), slug_fragment(&description)),
            kind,
            subject: PlanSubject {
                description: Some(description),
                ..PlanSubject::default()
            },
            mode: None,
            inputs: HashMap::new(),
            policy: HashMap::new(),
            steps: Vec::new(),
            artifacts: Vec::new(),
            proof: None,
            summary: None,
            warnings: Vec::new(),
            hints: Vec::new(),
        }
    }

    pub fn builder_for_component(kind: PlanKind, component_id: impl Into<String>) -> PlanBuilder {
        PlanBuilder::from_plan(Self::for_component(kind, component_id))
    }

    pub fn builder_for_description(kind: PlanKind, description: impl Into<String>) -> PlanBuilder {
        PlanBuilder::from_plan(Self::for_description(kind, description))
    }
}

impl Default for HomeboyPlan {
    fn default() -> Self {
        Self::for_description(PlanKind::Custom, "unspecified")
    }
}

/// High-level plan family. `Custom` gives extensions and future command
/// families a stable escape hatch without changing the schema shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    Quality,
    Build,
    Release,
    Deploy,
    DependencyStack,
    PullRequest,
    Ci,
    LabOffload,
    Trace,
    Review,
    Controller,
    AgentTaskFanout,
    AgentTaskMatrix,
    AgentTask,
    Custom,
}

impl PlanKind {
    fn slug(&self) -> &'static str {
        match self {
            Self::Quality => "quality",
            Self::Build => "build",
            Self::Release => "release",
            Self::Deploy => "deploy",
            Self::DependencyStack => "dependency_stack",
            Self::PullRequest => "pull_request",
            Self::Ci => "ci",
            Self::LabOffload => "lab_offload",
            Self::Trace => "trace",
            Self::Review => "review",
            Self::Controller => "controller",
            Self::AgentTaskFanout => "agent_task_fanout",
            Self::AgentTaskMatrix => "agent_task_matrix",
            Self::AgentTask => "agent_task",
            Self::Custom => "custom",
        }
    }
}

/// The thing a plan is about: a component, a file/symbol scope, or a prose
/// description when no stronger subject type exists yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSubject {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One planned unit of work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanStep {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_blocking")]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "PlanStepDependencyKind::is_default")]
    pub needs_kind: PlanStepDependencyKind,
    pub status: PlanStepStatus,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub outputs: HashMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub policy: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

/// Describes whether `needs` blocks execution or only preserves UI order.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepDependencyKind {
    /// The step consumes state or pass/fail results from the listed steps.
    #[default]
    Execution,
    /// The listed steps should render first, but do not block independent execution.
    DisplayOrder,
}

impl PlanStepDependencyKind {
    fn is_default(&self) -> bool {
        *self == Self::Execution
    }
}

impl PlanStep {
    pub fn builder(
        id: impl Into<String>,
        kind: impl Into<String>,
        status: PlanStepStatus,
    ) -> PlanStepBuilder {
        PlanStepBuilder::new(id, kind, status)
    }

    pub fn ready(id: impl Into<String>, kind: impl Into<String>) -> PlanStepBuilder {
        PlanStepBuilder::new(id, kind, PlanStepStatus::Ready)
    }

    pub fn ready_labeled(
        id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
        needs: impl IntoIterator<Item = String>,
        inputs: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Self {
        Self::ready(id, kind)
            .label(label)
            .needs(needs)
            .inputs(inputs)
            .build()
    }

    pub fn disabled(id: impl Into<String>, kind: impl Into<String>) -> PlanStepBuilder {
        PlanStepBuilder::new(id, kind, PlanStepStatus::Disabled)
    }

    pub fn disabled_with_reason(
        id: impl Into<String>,
        kind: impl Into<String>,
        reason: impl Into<String>,
    ) -> PlanStepBuilder {
        let reason = reason.into();
        Self::disabled(id, kind)
            .input_value("reason", serde_json::Value::String(reason.clone()))
            .skip_reason(reason)
    }

    pub fn input_as<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.inputs
            .get(key)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }
}

#[derive(Debug, Clone)]
pub struct PlanBuilder {
    plan: HomeboyPlan,
    summary_mode: SummaryMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryMode {
    None,
    Standard,
    DisabledAsSkipped,
}

impl PlanBuilder {
    pub(crate) fn from_plan(plan: HomeboyPlan) -> Self {
        Self {
            plan,
            summary_mode: SummaryMode::None,
        }
    }

    pub fn mode(mut self, mode: impl Into<String>) -> Self {
        self.plan.mode = Some(mode.into());
        self
    }

    pub fn inputs(mut self, inputs: impl IntoIterator<Item = (String, serde_json::Value)>) -> Self {
        self.plan.inputs.extend(inputs);
        self
    }

    pub fn policy_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.plan.policy.insert(key.into(), value);
        self
    }

    pub fn warnings(mut self, warnings: impl IntoIterator<Item = String>) -> Self {
        self.plan.warnings.extend(warnings);
        self
    }

    pub fn steps(mut self, steps: impl IntoIterator<Item = PlanStep>) -> Self {
        self.plan.steps.extend(steps);
        self
    }

    pub fn summarize(mut self) -> Self {
        self.summary_mode = SummaryMode::Standard;
        self
    }

    pub fn summarize_disabled_as_skipped(mut self) -> Self {
        self.summary_mode = SummaryMode::DisabledAsSkipped;
        self
    }

    pub fn build(mut self) -> HomeboyPlan {
        match self.summary_mode {
            SummaryMode::None => {}
            SummaryMode::Standard => {
                self.plan.summary = Some(PlanSummary::from_steps(&self.plan.steps));
            }
            SummaryMode::DisabledAsSkipped => {
                self.plan.summary = Some(PlanSummary::from_steps_counting_disabled_as_skipped(
                    &self.plan.steps,
                ));
            }
        }
        self.plan
    }
}

#[derive(Debug, Clone)]
pub struct PlanStepBuilder {
    step: PlanStep,
}

impl PlanStepBuilder {
    pub fn new(id: impl Into<String>, kind: impl Into<String>, status: PlanStepStatus) -> Self {
        Self {
            step: PlanStep {
                id: id.into(),
                kind: kind.into(),
                label: None,
                blocking: true,
                scope: Vec::new(),
                needs: Vec::new(),
                needs_kind: PlanStepDependencyKind::Execution,
                status,
                inputs: HashMap::new(),
                outputs: HashMap::new(),
                skip_reason: None,
                policy: HashMap::new(),
                missing: Vec::new(),
            },
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.step.label = Some(label.into());
        self
    }

    pub fn blocking(mut self, blocking: bool) -> Self {
        self.step.blocking = blocking;
        self
    }

    pub fn scope(mut self, scope: impl IntoIterator<Item = String>) -> Self {
        self.step.scope.extend(scope);
        self
    }

    pub fn needs(mut self, needs: impl IntoIterator<Item = String>) -> Self {
        self.step.needs.extend(needs);
        self
    }

    pub fn needs_kind(mut self, needs_kind: PlanStepDependencyKind) -> Self {
        self.step.needs_kind = needs_kind;
        self
    }

    pub fn input_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.step.inputs.insert(key.into(), value);
        self
    }

    pub fn inputs(mut self, inputs: impl IntoIterator<Item = (String, serde_json::Value)>) -> Self {
        self.step.inputs.extend(inputs);
        self
    }

    pub fn output_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.step.outputs.insert(key.into(), value);
        self
    }

    pub fn gate_result(mut self, gate_result: impl Into<HomeboyGateResult>) -> Self {
        self.step.outputs.insert(
            "gate_result".to_string(),
            serde_json::to_value(gate_result.into()).unwrap_or(serde_json::Value::Null),
        );
        self
    }

    pub fn missing(mut self, missing: impl IntoIterator<Item = String>) -> Self {
        self.step.missing.extend(missing);
        self
    }

    pub fn skip_reason(mut self, reason: impl Into<String>) -> Self {
        self.step.skip_reason = Some(reason.into());
        self
    }

    pub fn build(self) -> PlanStep {
        self.step
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Ready,
    Missing,
    Disabled,
    Skipped,
    Running,
    Success,
    PartialSuccess,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanArtifact {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub data: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSummary {
    pub total_steps: usize,
    pub ready: usize,
    pub blocked: usize,
    pub skipped: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}

impl PlanSummary {
    pub fn from_steps(steps: &[PlanStep]) -> Self {
        Self::from_steps_with_skipped_statuses(steps, &[PlanStepStatus::Skipped])
    }

    pub fn from_steps_counting_disabled_as_skipped(steps: &[PlanStep]) -> Self {
        Self::from_steps_with_skipped_statuses(
            steps,
            &[PlanStepStatus::Skipped, PlanStepStatus::Disabled],
        )
    }

    fn from_steps_with_skipped_statuses(
        steps: &[PlanStep],
        skipped_statuses: &[PlanStepStatus],
    ) -> Self {
        Self {
            total_steps: steps.len(),
            ready: steps
                .iter()
                .filter(|step| step.status == PlanStepStatus::Ready)
                .count(),
            blocked: steps
                .iter()
                .filter(|step| step.status == PlanStepStatus::Missing)
                .count(),
            skipped: steps
                .iter()
                .filter(|step| skipped_statuses.contains(&step.status))
                .count(),
            next_actions: Vec::new(),
        }
    }
}

fn default_blocking() -> bool {
    true
}

fn slug_fragment(value: &str) -> String {
    use homeboy_engine_primitives::identifier::{slugify, SlugStyle};
    let slug = slugify(value, SlugStyle::Branch);

    if slug.is_empty() {
        "unspecified".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanSummary};

    #[test]
    fn serializes_minimal_component_plan() {
        let plan = HomeboyPlan::for_component(PlanKind::Release, "homeboy");

        let serialized = serde_json::to_value(&plan).expect("serialize plan");

        assert_eq!(
            serialized,
            serde_json::json!({
                "id": "release.homeboy",
                "kind": "release",
                "subject": {
                    "component_id": "homeboy"
                }
            })
        );
    }

    #[test]
    fn for_description_slugs_the_description_into_the_plan_id() {
        let plan = HomeboyPlan::for_description(PlanKind::Trace, "Variant A/B");

        assert_eq!(plan.id, "trace.variant-a-b");
        assert_eq!(plan.kind, PlanKind::Trace);
        assert_eq!(plan.subject.description.as_deref(), Some("Variant A/B"));
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn deserializes_default_blocking_step() {
        let step: PlanStep = serde_json::from_value(serde_json::json!({
            "id": "lint",
            "kind": "quality.lint",
            "status": "ready"
        }))
        .expect("deserialize step");

        assert!(step.blocking);
        assert!(step.scope.is_empty());
        assert!(step.inputs.is_empty());
    }

    #[test]
    fn builder_generates_summary_from_step_statuses() {
        let steps = || {
            vec![
                PlanStep::ready("quality.lint", "quality.lint").build(),
                PlanStep::builder("quality.test", "quality.test", PlanStepStatus::Missing).build(),
                PlanStep::builder("quality.audit", "quality.audit", PlanStepStatus::Skipped)
                    .build(),
                PlanStep::disabled("quality.docs", "quality.docs").build(),
            ]
        };
        let summary = |skipped| PlanSummary {
            total_steps: 4,
            ready: 1,
            blocked: 1,
            skipped,
            next_actions: Vec::new(),
        };

        let standard = HomeboyPlan::builder_for_component(PlanKind::Quality, "fixture")
            .steps(steps())
            .summarize()
            .build();
        assert_eq!(standard.summary, Some(summary(1)));

        let disabled_as_skipped = HomeboyPlan::builder_for_component(PlanKind::Quality, "fixture")
            .steps(steps())
            .summarize_disabled_as_skipped()
            .build();
        assert_eq!(disabled_as_skipped.summary, Some(summary(2)));
    }

    #[test]
    fn input_as_decodes_typed_inputs_and_rejects_missing_or_mismatched() {
        let step = PlanStep::ready("refactor", "refactor.decompose")
            .input_value("items", serde_json::json!(["first", "second"]))
            .build();

        let items: Option<Vec<String>> = step.input_as("items");
        let missing: Option<Vec<String>> = step.input_as("missing");
        let wrong_type: Option<bool> = step.input_as("items");

        assert_eq!(items, Some(vec!["first".to_string(), "second".to_string()]));
        assert_eq!(missing, None);
        assert_eq!(wrong_type, None);
    }
}
