//! Generic typed plan substrate for Homeboy workflows.
//!
//! Domain-specific planners can keep their existing public JSON contracts while
//! adapting toward this shared shape. The common model answers what will run,
//! why it will run, whether it blocks progress, and what artifacts/results are
//! expected.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PlanValues {
    values: HashMap<String, serde_json::Value>,
}

impl PlanValues {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn string(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.values
            .insert(key.into(), serde_json::Value::String(value.into()));
        self
    }

    pub(crate) fn bool(mut self, key: impl Into<String>, value: bool) -> Self {
        self.values
            .insert(key.into(), serde_json::Value::Bool(value));
        self
    }

    pub(crate) fn number(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Number>,
    ) -> Self {
        self.values
            .insert(key.into(), serde_json::Value::Number(value.into()));
        self
    }

    pub(crate) fn json<T: Serialize>(mut self, key: impl Into<String>, value: T) -> Self {
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
            summary: None,
            warnings: Vec::new(),
            hints: Vec::new(),
        }
    }

    pub(crate) fn builder_for_component(
        kind: PlanKind,
        component_id: impl Into<String>,
    ) -> PlanBuilder {
        PlanBuilder::from_plan(Self::for_component(kind, component_id))
    }

    pub(crate) fn builder_for_description(
        kind: PlanKind,
        description: impl Into<String>,
    ) -> PlanBuilder {
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
    Audit,
    Quality,
    Build,
    Release,
    Deploy,
    DependencyStack,
    IssueReconcile,
    PullRequest,
    Ci,
    Refactor,
    StackSync,
    Trace,
    Review,
    Custom,
}

impl PlanKind {
    fn slug(&self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::Quality => "quality",
            Self::Build => "build",
            Self::Release => "release",
            Self::Deploy => "deploy",
            Self::DependencyStack => "dependency_stack",
            Self::IssueReconcile => "issue_reconcile",
            Self::PullRequest => "pull_request",
            Self::Ci => "ci",
            Self::Refactor => "refactor",
            Self::StackSync => "stack_sync",
            Self::Trace => "trace",
            Self::Review => "review",
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

impl PlanStep {
    pub(crate) fn builder(
        id: impl Into<String>,
        kind: impl Into<String>,
        status: PlanStepStatus,
    ) -> PlanStepBuilder {
        PlanStepBuilder::new(id, kind, status)
    }

    pub(crate) fn ready(id: impl Into<String>, kind: impl Into<String>) -> PlanStepBuilder {
        PlanStepBuilder::new(id, kind, PlanStepStatus::Ready)
    }

    pub(crate) fn ready_labeled(
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

    pub(crate) fn disabled(id: impl Into<String>, kind: impl Into<String>) -> PlanStepBuilder {
        PlanStepBuilder::new(id, kind, PlanStepStatus::Disabled)
    }

    pub(crate) fn disabled_with_reason(
        id: impl Into<String>,
        kind: impl Into<String>,
        reason: impl Into<String>,
    ) -> PlanStepBuilder {
        let reason = reason.into();
        Self::disabled(id, kind)
            .input_value("reason", serde_json::Value::String(reason.clone()))
            .skip_reason(reason)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PlanBuilder {
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

    pub(crate) fn mode(mut self, mode: impl Into<String>) -> Self {
        self.plan.mode = Some(mode.into());
        self
    }

    pub(crate) fn inputs(
        mut self,
        inputs: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Self {
        self.plan.inputs.extend(inputs);
        self
    }

    pub(crate) fn policy_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.plan.policy.insert(key.into(), value);
        self
    }

    pub(crate) fn warnings(mut self, warnings: impl IntoIterator<Item = String>) -> Self {
        self.plan.warnings.extend(warnings);
        self
    }

    pub(crate) fn steps(mut self, steps: impl IntoIterator<Item = PlanStep>) -> Self {
        self.plan.steps.extend(steps);
        self
    }

    pub(crate) fn summarize(mut self) -> Self {
        self.summary_mode = SummaryMode::Standard;
        self
    }

    pub(crate) fn summarize_disabled_as_skipped(mut self) -> Self {
        self.summary_mode = SummaryMode::DisabledAsSkipped;
        self
    }

    pub(crate) fn build(mut self) -> HomeboyPlan {
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
pub(crate) struct PlanStepBuilder {
    step: PlanStep,
}

impl PlanStepBuilder {
    pub(crate) fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        status: PlanStepStatus,
    ) -> Self {
        Self {
            step: PlanStep {
                id: id.into(),
                kind: kind.into(),
                label: None,
                blocking: true,
                scope: Vec::new(),
                needs: Vec::new(),
                status,
                inputs: HashMap::new(),
                outputs: HashMap::new(),
                skip_reason: None,
                policy: HashMap::new(),
                missing: Vec::new(),
            },
        }
    }

    pub(crate) fn label(mut self, label: impl Into<String>) -> Self {
        self.step.label = Some(label.into());
        self
    }

    pub(crate) fn blocking(mut self, blocking: bool) -> Self {
        self.step.blocking = blocking;
        self
    }

    pub(crate) fn scope(mut self, scope: impl IntoIterator<Item = String>) -> Self {
        self.step.scope.extend(scope);
        self
    }

    pub(crate) fn needs(mut self, needs: impl IntoIterator<Item = String>) -> Self {
        self.step.needs.extend(needs);
        self
    }

    pub(crate) fn input_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.step.inputs.insert(key.into(), value);
        self
    }

    pub(crate) fn inputs(
        mut self,
        inputs: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Self {
        self.step.inputs.extend(inputs);
        self
    }

    pub(crate) fn skip_reason(mut self, reason: impl Into<String>) -> Self {
        self.step.skip_reason = Some(reason.into());
        self
    }

    pub(crate) fn build(self) -> PlanStep {
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
    pub(crate) fn from_steps(steps: &[PlanStep]) -> Self {
        Self::from_steps_with_skipped_statuses(steps, &[PlanStepStatus::Skipped])
    }

    pub(crate) fn from_steps_counting_disabled_as_skipped(steps: &[PlanStep]) -> Self {
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
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        "unspecified".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HomeboyPlan, PlanBuilder, PlanKind, PlanStep, PlanStepStatus, PlanSummary, PlanValues,
    };

    #[test]
    fn serializes_plan_kind_as_snake_case() {
        let serialized = serde_json::to_value(PlanKind::PullRequest).expect("serialize plan kind");

        assert_eq!(serialized, serde_json::json!("pull_request"));
    }

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
    fn test_for_component() {
        let plan = HomeboyPlan::for_component(PlanKind::Quality, "fixture");

        assert_eq!(plan.id, "quality.fixture");
        assert_eq!(plan.kind, PlanKind::Quality);
        assert_eq!(plan.subject.component_id.as_deref(), Some("fixture"));
        assert!(plan.steps.is_empty());
        assert!(plan.warnings.is_empty());
        assert!(plan.hints.is_empty());
    }

    #[test]
    fn test_for_description() {
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
    fn serializes_step_status_as_snake_case() {
        let serialized =
            serde_json::to_value(PlanStepStatus::PartialSuccess).expect("serialize step status");

        assert_eq!(serialized, serde_json::json!("partial_success"));
    }

    #[test]
    fn builder_generates_summary_from_step_statuses() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Quality, "fixture")
            .steps(vec![
                PlanStep::ready("quality.lint", "quality.lint").build(),
                PlanStep::builder("quality.test", "quality.test", PlanStepStatus::Missing).build(),
                PlanStep::builder("quality.audit", "quality.audit", PlanStepStatus::Skipped)
                    .build(),
                PlanStep::disabled("quality.docs", "quality.docs").build(),
            ])
            .summarize()
            .build();

        assert_eq!(
            plan.summary,
            Some(PlanSummary {
                total_steps: 4,
                ready: 1,
                blocked: 1,
                skipped: 1,
                next_actions: Vec::new(),
            })
        );
    }

    #[test]
    fn step_builder_preserves_minimal_step_json_shape() {
        let step = PlanStep::ready("lint", "quality.lint")
            .label("Run lint")
            .blocking(false)
            .needs(vec!["audit".to_string()])
            .input_value("reason", serde_json::json!("manual"))
            .skip_reason("manual")
            .build();

        let serialized = serde_json::to_value(&step).expect("serialize step");

        assert_eq!(
            serialized,
            serde_json::json!({
                "id": "lint",
                "kind": "quality.lint",
                "label": "Run lint",
                "blocking": false,
                "needs": ["audit"],
                "status": "ready",
                "inputs": {
                    "reason": "manual"
                },
                "skip_reason": "manual"
            })
        );
    }

    #[test]
    fn test_builder_for_component() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Quality, "fixture").build();

        assert_eq!(plan.subject.component_id.as_deref(), Some("fixture"));
    }

    #[test]
    fn test_builder_for_description() {
        let plan = HomeboyPlan::builder_for_description(PlanKind::Trace, "Variant A").build();

        assert_eq!(plan.subject.description.as_deref(), Some("Variant A"));
    }

    #[test]
    fn test_from_plan() {
        let plan =
            PlanBuilder::from_plan(HomeboyPlan::for_component(PlanKind::Build, "fixture")).build();

        assert_eq!(plan.kind, PlanKind::Build);
    }

    #[test]
    fn test_mode() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Review, "fixture")
            .mode("changed")
            .build();

        assert_eq!(plan.mode.as_deref(), Some("changed"));
    }

    #[test]
    fn test_plan_inputs() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Trace, "fixture")
            .inputs(vec![
                ("group".to_string(), serde_json::json!("baseline")),
                ("iteration".to_string(), serde_json::json!(2)),
            ])
            .build();

        assert_eq!(
            plan.inputs.get("group"),
            Some(&serde_json::json!("baseline"))
        );
        assert_eq!(plan.inputs.get("iteration"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn test_string() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Trace, "fixture")
            .inputs(PlanValues::new().string("group", "baseline"))
            .build();

        assert_eq!(
            plan.inputs.get("group"),
            Some(&serde_json::json!("baseline"))
        );
    }

    #[test]
    fn test_bool() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Trace, "fixture")
            .inputs(PlanValues::new().bool("dry_run", true))
            .build();

        assert_eq!(plan.inputs.get("dry_run"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn test_number() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Trace, "fixture")
            .inputs(PlanValues::new().number("iteration", 2_u64))
            .build();

        assert_eq!(plan.inputs.get("iteration"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn test_plan_values_json() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Trace, "fixture")
            .inputs(PlanValues::new().json("items", ["first", "second"]))
            .build();

        assert_eq!(
            plan.inputs.get("items"),
            Some(&serde_json::json!(["first", "second"]))
        );
    }

    #[test]
    fn test_input_value() {
        let step = PlanStep::ready("lint", "quality.lint")
            .input_value("reason", serde_json::json!("manual"))
            .build();

        assert_eq!(
            step.inputs.get("reason"),
            Some(&serde_json::json!("manual"))
        );
    }

    #[test]
    fn test_policy_value() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::StackSync, "fixture")
            .policy_value("blocked", serde_json::json!(false))
            .build();

        assert_eq!(plan.policy.get("blocked"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn test_warnings() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Refactor, "fixture")
            .warnings(vec!["review grouping".to_string()])
            .build();

        assert_eq!(plan.warnings, vec!["review grouping"]);
    }

    #[test]
    fn test_steps() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Quality, "fixture")
            .steps(vec![PlanStep::ready("lint", "quality.lint").build()])
            .build();

        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn test_summarize() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Quality, "fixture")
            .steps(vec![PlanStep::ready("lint", "quality.lint").build()])
            .summarize()
            .build();

        assert_eq!(plan.summary.as_ref().map(|summary| summary.ready), Some(1));
    }

    #[test]
    fn test_summarize_disabled_as_skipped() {
        let plan = HomeboyPlan::builder_for_component(PlanKind::Audit, "fixture")
            .steps(vec![PlanStep::disabled(
                "audit.docs",
                "audit.detector.docs",
            )
            .build()])
            .summarize_disabled_as_skipped()
            .build();

        assert_eq!(
            plan.summary.as_ref().map(|summary| summary.skipped),
            Some(1)
        );
    }

    #[test]
    fn test_ready() {
        let step = PlanStep::ready("lint", "quality.lint").build();

        assert_eq!(step.status, PlanStepStatus::Ready);
    }

    #[test]
    fn test_ready_labeled() {
        let step = PlanStep::ready_labeled(
            "test",
            "quality.test",
            "Run tests",
            vec!["lint".to_string()],
            vec![("profile".to_string(), serde_json::json!("ci"))],
        );

        assert_eq!(step.status, PlanStepStatus::Ready);
        assert_eq!(step.label.as_deref(), Some("Run tests"));
        assert_eq!(step.needs, vec!["lint"]);
        assert_eq!(step.inputs.get("profile"), Some(&serde_json::json!("ci")));
    }

    #[test]
    fn test_disabled() {
        let step = PlanStep::disabled("audit", "quality.audit").build();

        assert_eq!(step.status, PlanStepStatus::Disabled);
    }

    #[test]
    fn test_label() {
        let step = PlanStep::ready("lint", "quality.lint")
            .label("Run lint")
            .build();

        assert_eq!(step.label.as_deref(), Some("Run lint"));
    }

    #[test]
    fn test_scope() {
        let step = PlanStep::ready("deps", "deps.stack")
            .scope(vec!["downstream".to_string()])
            .build();

        assert_eq!(step.scope, vec!["downstream"]);
    }

    #[test]
    fn test_needs() {
        let step = PlanStep::ready("test", "quality.test")
            .needs(vec!["lint".to_string()])
            .build();

        assert_eq!(step.needs, vec!["lint"]);
    }

    #[test]
    fn test_inputs() {
        let step = PlanStep::ready("deps", "deps.stack")
            .inputs(vec![("package".to_string(), serde_json::json!("homeboy"))])
            .build();

        assert_eq!(
            step.inputs.get("package"),
            Some(&serde_json::json!("homeboy"))
        );
    }

    #[test]
    fn test_skip_reason() {
        let step = PlanStep::disabled("audit", "quality.audit")
            .skip_reason("filtered")
            .build();

        assert_eq!(step.skip_reason.as_deref(), Some("filtered"));
    }

    #[test]
    fn test_disabled_with_reason() {
        let step = PlanStep::disabled_with_reason("release.skip", "release.skip", "no-commits")
            .label("No releasable commits")
            .build();

        assert_eq!(step.status, PlanStepStatus::Disabled);
        assert_eq!(step.skip_reason.as_deref(), Some("no-commits"));
        assert_eq!(
            step.inputs.get("reason").and_then(|value| value.as_str()),
            Some("no-commits")
        );
    }

    #[test]
    fn test_from_steps() {
        let summary = PlanSummary::from_steps(&[
            PlanStep::ready("lint", "quality.lint").build(),
            PlanStep::builder("test", "quality.test", PlanStepStatus::Missing).build(),
        ]);

        assert_eq!(summary.ready, 1);
        assert_eq!(summary.blocked, 1);
    }

    #[test]
    fn test_from_steps_counting_disabled_as_skipped() {
        let summary = PlanSummary::from_steps_counting_disabled_as_skipped(&[PlanStep::disabled(
            "audit",
            "audit.detector.docs",
        )
        .build()]);

        assert_eq!(summary.skipped, 1);
    }
}
