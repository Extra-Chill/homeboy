use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep};

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
    pub skip_audit: bool,
    pub skip_lint: bool,
    pub skip_test: bool,
    pub granular_skip_reason: String,
    pub lint_needs: Vec<String>,
    pub test_needs: Vec<String>,
    pub audit_policy_available: bool,
    pub audit_label: String,
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
            skip_audit: false,
            skip_lint: false,
            skip_test: false,
            granular_skip_reason: "--skip-checks=<check>".to_string(),
            lint_needs: vec!["preflight.bump_policy".to_string()],
            test_needs: vec!["preflight.lint".to_string()],
            audit_policy_available: false,
            audit_label: "Run release audit".to_string(),
            lint_label: "Run release lint".to_string(),
            test_label: "Run release tests".to_string(),
        }
    }

    /// Apply granular per-check skips from a list of check names.
    ///
    /// Recognized names: `audit`, `lint`, `test`. Unknown names are ignored by
    /// this method (validation happens at the CLI boundary).
    pub fn with_granular_skips(mut self, checks: &[String]) -> Self {
        for check in checks {
            match check.to_ascii_lowercase().as_str() {
                "audit" => self.skip_audit = true,
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
            skip_audit: false,
            skip_lint: false,
            skip_test: false,
            granular_skip_reason: "--skip-checks=<check>".to_string(),
            lint_needs: vec!["review.audit".to_string()],
            test_needs: vec!["review.lint".to_string()],
            audit_policy_available: true,
            audit_label: "Run review audit".to_string(),
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
                "audit",
                &options.audit_label,
                &options.skip_reason,
            ),
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

    let audit = if options.skip_audit {
        disabled_step(
            &options.step_prefix,
            "audit",
            &options.audit_label,
            &options.granular_skip_reason,
        )
    } else if options.audit_policy_available {
        ready_step(
            &options.step_prefix,
            "audit",
            &options.audit_label,
            Vec::new(),
        )
    } else {
        disabled_step(
            &options.step_prefix,
            "audit",
            &options.audit_label,
            "no-release-audit-policy",
        )
    };

    let lint = if options.skip_lint {
        disabled_step(
            &options.step_prefix,
            "lint",
            &options.lint_label,
            &options.granular_skip_reason,
        )
    } else {
        ready_step(
            &options.step_prefix,
            "lint",
            &options.lint_label,
            options.lint_needs.clone(),
        )
    };

    let test = if options.skip_test {
        disabled_step(
            &options.step_prefix,
            "test",
            &options.test_label,
            &options.granular_skip_reason,
        )
    } else {
        ready_step(
            &options.step_prefix,
            "test",
            &options.test_label,
            options.test_needs.clone(),
        )
    };

    vec![audit, lint, test]
}

fn ready_step(prefix: &str, name: &str, label: &str, needs: Vec<String>) -> PlanStep {
    let id = step_id(prefix, name);
    PlanStep::ready_labeled(
        id.clone(),
        id,
        label,
        needs,
        std::iter::empty::<(String, serde_json::Value)>(),
    )
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
    use crate::core::plan::{PlanKind, PlanStepStatus};

    #[test]
    fn test_release_preflight() {
        let options = QualityPlanOptions::release_preflight("fixture", false);

        assert_eq!(options.component_id, "fixture");
        assert_eq!(options.mode.as_deref(), Some("release-preflight"));
        assert_eq!(options.step_prefix, "preflight");
        assert_eq!(options.skip_reason, "--skip-checks");
        assert_eq!(options.lint_needs, vec!["preflight.bump_policy"]);
        assert_eq!(options.test_needs, vec!["preflight.lint"]);
        assert!(!options.audit_policy_available);
    }

    #[test]
    fn test_review() {
        let options = QualityPlanOptions::review("fixture");

        assert_eq!(options.component_id, "fixture");
        assert_eq!(options.mode.as_deref(), Some("review"));
        assert_eq!(options.step_prefix, "review");
        assert_eq!(options.lint_needs, vec!["review.audit"]);
        assert_eq!(options.test_needs, vec!["review.lint"]);
        assert!(options.audit_policy_available);
    }

    #[test]
    fn test_skipped_review() {
        let plan = build_quality_plan(QualityPlanOptions::skipped_review(
            "fixture",
            "no files changed",
        ));

        assert_eq!(plan.mode.as_deref(), Some("review"));
        assert!(plan
            .steps
            .iter()
            .all(|step| step.status == PlanStepStatus::Disabled));
        assert!(plan.steps.iter().all(|step| step
            .inputs
            .get("reason")
            .and_then(|value| value.as_str())
            == Some("no files changed")));
    }

    #[test]
    fn test_build_quality_plan() {
        let plan = build_quality_plan(QualityPlanOptions::release_preflight("fixture", false));
        let ids: Vec<&str> = plan.steps.iter().map(|step| step.id.as_str()).collect();

        assert_eq!(plan.kind, PlanKind::Quality);
        assert_eq!(plan.subject.component_id.as_deref(), Some("fixture"));
        assert_eq!(plan.mode.as_deref(), Some("release-preflight"));
        assert_eq!(
            ids,
            vec!["preflight.audit", "preflight.lint", "preflight.test"]
        );
        assert_eq!(plan.steps[0].status, PlanStepStatus::Disabled);
        assert_eq!(plan.steps[1].needs, vec!["preflight.bump_policy"]);
        assert_eq!(plan.steps[2].needs, vec!["preflight.lint"]);
    }

    #[test]
    fn test_build_quality_steps() {
        let options = QualityPlanOptions::release_preflight("fixture", true);
        let steps = build_quality_steps(&options);

        assert!(steps
            .iter()
            .all(|step| step.status == PlanStepStatus::Disabled));
        assert!(steps.iter().all(|step| step
            .inputs
            .get("reason")
            .and_then(|value| value.as_str())
            == Some("--skip-checks")));
    }

    #[test]
    fn granular_skip_disables_only_named_checks() {
        // --skip-checks=lint disables only lint; audit and test stay active.
        let options = QualityPlanOptions::release_preflight("fixture", false)
            .with_granular_skips(&["lint".to_string()]);
        let steps = build_quality_steps(&options);

        let lint = steps
            .iter()
            .find(|s| s.id == "preflight.lint")
            .expect("lint step");
        assert_eq!(lint.status, PlanStepStatus::Disabled);

        let test = steps
            .iter()
            .find(|s| s.id == "preflight.test")
            .expect("test step");
        assert_ne!(test.status, PlanStepStatus::Disabled);
    }

    #[test]
    fn granular_skip_accepts_multiple_checks_case_insensitively() {
        let options = QualityPlanOptions::release_preflight("fixture", false)
            .with_granular_skips(&["LINT".to_string(), "Tests".to_string()]);
        assert!(options.skip_lint);
        assert!(options.skip_test);
        assert!(!options.skip_audit);
    }

    #[test]
    fn granular_skip_does_not_affect_skip_checks_all() {
        // When skip_checks (all) is true, granular flags are irrelevant — every
        // gate is disabled by the blanket skip.
        let options = QualityPlanOptions::release_preflight("fixture", true)
            .with_granular_skips(&["lint".to_string()]);
        let steps = build_quality_steps(&options);

        assert!(steps
            .iter()
            .all(|step| step.status == PlanStepStatus::Disabled));
    }
}
