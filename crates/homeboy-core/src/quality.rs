use crate::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepDependencyKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityPlanOptions {
    pub component_id: String,
    pub mode: Option<String>,
    pub step_prefix: String,
    pub skip_checks: bool,
    pub skip_reason: String,
    /// Granular per-check skips. When set, only the listed checks are disabled
    /// while the rest still run. Honored independently of `skip_checks` (which
    /// skips everything). Used by `--skip-checks=lint` to bypass only the broken
    /// gate without disabling working_tree/remote_sync/test.
    pub skip_lint: bool,
    pub skip_test: bool,
    pub granular_skip_reason: String,
    pub lint_needs: Vec<String>,
    pub test_needs: Vec<String>,
    pub needs_kind: PlanStepDependencyKind,
    pub lint_label: String,
    pub test_label: String,
}

impl QualityPlanOptions {
    pub fn release_preflight(component_id: impl Into<String>, skip_checks: bool) -> Self {
        Self {
            component_id: component_id.into(),
            mode: Some("release-preflight".to_string()),
            step_prefix: "preflight".to_string(),
            skip_checks,
            skip_reason: "--skip-checks".to_string(),
            skip_lint: false,
            skip_test: false,
            granular_skip_reason: "--skip-checks=<check>".to_string(),
            lint_needs: vec!["preflight.bump_policy".to_string()],
            test_needs: vec!["preflight.lint".to_string()],
            needs_kind: PlanStepDependencyKind::Execution,
            lint_label: "Run release lint".to_string(),
            test_label: "Run release tests".to_string(),
        }
    }

    /// Apply granular per-check skips from a list of check names.
    ///
    /// Recognized names: `lint`, `test`. Unknown names are ignored by
    /// this method (validation happens at the CLI boundary).
    pub fn with_granular_skips(mut self, checks: &[String]) -> Self {
        for check in checks {
            match check.to_ascii_lowercase().as_str() {
                "lint" => self.skip_lint = true,
                "test" | "tests" => self.skip_test = true,
                _ => {}
            }
        }
        self
    }

    pub fn review(component_id: impl Into<String>) -> Self {
        Self {
            component_id: component_id.into(),
            mode: Some("review".to_string()),
            step_prefix: "review".to_string(),
            skip_checks: false,
            skip_reason: "skipped".to_string(),
            skip_lint: false,
            skip_test: false,
            granular_skip_reason: "--skip-checks=<check>".to_string(),
            lint_needs: Vec::new(),
            test_needs: vec!["review.lint".to_string()],
            needs_kind: PlanStepDependencyKind::DisplayOrder,
            lint_label: "Run review lint".to_string(),
            test_label: "Run review tests".to_string(),
        }
    }

    pub fn skipped_review(component_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            skip_checks: true,
            skip_reason: reason.into(),
            ..Self::review(component_id)
        }
    }
}

pub fn build_quality_plan(options: QualityPlanOptions) -> HomeboyPlan {
    let steps = build_quality_steps(&options);
    let mut builder =
        HomeboyPlan::builder_for_component(PlanKind::Quality, options.component_id).steps(steps);
    if let Some(mode) = options.mode {
        builder = builder.mode(mode);
    }
    builder.build()
}

pub fn build_quality_steps(options: &QualityPlanOptions) -> Vec<PlanStep> {
    if options.skip_checks {
        return vec![
            disabled_step(
                &options.step_prefix,
                "lint",
                &options.lint_label,
                &options.skip_reason,
            ),
            disabled_step(
                &options.step_prefix,
                "test",
                &options.test_label,
                &options.skip_reason,
            ),
        ];
    }

    let skippable_step = |skip: bool, name: &str, label: &str, needs: &[String]| {
        if skip {
            disabled_step(
                &options.step_prefix,
                name,
                label,
                &options.granular_skip_reason,
            )
        } else {
            ready_step(
                &options.step_prefix,
                name,
                label,
                needs.to_vec(),
                options.needs_kind,
            )
        }
    };

    let lint = skippable_step(
        options.skip_lint,
        "lint",
        &options.lint_label,
        &options.lint_needs,
    );

    let test = skippable_step(
        options.skip_test,
        "test",
        &options.test_label,
        &options.test_needs,
    );

    vec![lint, test]
}

fn ready_step(
    prefix: &str,
    name: &str,
    label: &str,
    needs: Vec<String>,
    needs_kind: PlanStepDependencyKind,
) -> PlanStep {
    let id = step_id(prefix, name);
    PlanStep::ready(id.clone(), id)
        .label(label)
        .needs(needs)
        .needs_kind(needs_kind)
        .build()
}

fn disabled_step(prefix: &str, name: &str, label: &str, reason: &str) -> PlanStep {
    let id = step_id(prefix, name);
    PlanStep::disabled_with_reason(id.clone(), id, reason)
        .label(label)
        .build()
}

fn step_id(prefix: &str, name: &str) -> String {
    format!("{prefix}.{name}")
}

#[cfg(test)]
mod tests {
    use super::{build_quality_plan, build_quality_steps, QualityPlanOptions};
    use crate::plan::{PlanKind, PlanStepDependencyKind, PlanStepStatus};

    #[test]
    fn blanket_skip_disables_every_check_with_its_reason() {
        for (options, reason) in [
            (
                QualityPlanOptions::skipped_review("fixture", "no files changed"),
                "no files changed",
            ),
            (
                QualityPlanOptions::release_preflight("fixture", true),
                "--skip-checks",
            ),
            (
                QualityPlanOptions::release_preflight("fixture", true)
                    .with_granular_skips(&["lint".to_string()]),
                "--skip-checks",
            ),
        ] {
            let steps = build_quality_steps(&options);
            assert_eq!(steps.len(), 2);
            for step in steps {
                assert_eq!(step.status, PlanStepStatus::Disabled, "{}", step.id);
                assert_eq!(
                    step.inputs.get("reason").and_then(|value| value.as_str()),
                    Some(reason),
                    "{}",
                    step.id
                );
            }
        }
    }

    #[test]
    fn release_preflight_edges_execute_while_review_edges_only_order_display() {
        let plan = build_quality_plan(QualityPlanOptions::release_preflight("fixture", false));
        let ids: Vec<&str> = plan.steps.iter().map(|step| step.id.as_str()).collect();

        assert_eq!(plan.kind, PlanKind::Quality);
        assert_eq!(plan.subject.component_id.as_deref(), Some("fixture"));
        assert_eq!(plan.mode.as_deref(), Some("release-preflight"));
        assert_eq!(ids, vec!["preflight.lint", "preflight.test"]);
        assert_eq!(plan.steps[0].needs, vec!["preflight.bump_policy"]);
        assert_eq!(plan.steps[1].needs, vec!["preflight.lint"]);
        assert_eq!(plan.steps[0].needs_kind, PlanStepDependencyKind::Execution);

        let review = build_quality_plan(QualityPlanOptions::review("fixture"));
        assert!(review.steps[0].needs.is_empty());
        assert_eq!(review.steps[1].needs, vec!["review.lint"]);
        assert_eq!(
            review.steps[1].needs_kind,
            PlanStepDependencyKind::DisplayOrder
        );
    }

    #[test]
    fn granular_skip_disables_only_named_checks_case_insensitively() {
        for (checks, lint_disabled, test_disabled) in [
            (vec!["lint"], true, false),
            (vec!["LINT", "Tests"], true, true),
            (vec!["test", "unknown"], false, true),
        ] {
            let checks: Vec<String> = checks.into_iter().map(str::to_string).collect();
            let options = QualityPlanOptions::release_preflight("fixture", false)
                .with_granular_skips(&checks);
            let steps = build_quality_steps(&options);
            let disabled = |id: &str| {
                steps
                    .iter()
                    .find(|step| step.id == id)
                    .map(|step| step.status == PlanStepStatus::Disabled)
                    .expect(id)
            };
            assert_eq!(disabled("preflight.lint"), lint_disabled, "{checks:?}");
            assert_eq!(disabled("preflight.test"), test_disabled, "{checks:?}");
        }
    }
}
