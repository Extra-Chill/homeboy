//! Pure reconciliation for one rolling findings issue per component.

use std::collections::BTreeMap;

use super::plan::{
    IssueGroup, ReconcileAction, ReconcileConfig, ReconcilePlan, ReconcileSkipReason, TrackedIssue,
    TrackedIssueState,
};

pub(crate) const FINDINGS_KEY_PREFIX: &str = "homeboy:issues-reconcile-key=findings:";
const SECTION_PREFIX: &str = "<!-- homeboy:findings-section=";
const ISSUE_LABEL: &str = "homeboy-findings";

/// Reconcile one command's measurement into the component's rolling findings issue.
///
/// Every finding category is stored in an independently keyed section. Complete
/// measurements retire categories absent from the current command output;
/// narrowed measurements update only categories they explicitly measured.
pub fn reconcile_measured(
    groups: &[IssueGroup],
    existing: &[TrackedIssue],
    config: &ReconcileConfig,
    command: &str,
    component_id: &str,
    complete_measurement: bool,
) -> ReconcilePlan {
    let mut canonical: Vec<&TrackedIssue> = existing
        .iter()
        .filter(|issue| parse_canonical_component(&issue.body).as_deref() == Some(component_id))
        .collect();
    canonical.sort_by_key(|issue| issue.number);
    let mut sections = collect_sections(&canonical);
    merge_measurement(&mut sections, groups, command, complete_measurement);

    let open: Vec<&TrackedIssue> = canonical
        .iter()
        .copied()
        .filter(|issue| issue.state.is_open())
        .collect();
    let closed_not_planned = canonical
        .iter()
        .copied()
        .filter(|issue| issue.state == TrackedIssueState::ClosedNotPlanned)
        .max_by_key(|issue| issue.number);

    let mut actions = Vec::new();
    if sections.is_empty() {
        if let Some((keep, duplicates)) = open.split_first() {
            actions.push(ReconcileAction::Close {
                number: keep.number,
                comment: "All Homeboy findings have been resolved. Closing automatically."
                    .to_string(),
            });
            close_duplicates(&mut actions, duplicates, keep.number);
        } else {
            actions.push(ReconcileAction::Skip {
                component_id: component_id.to_string(),
                reason: ReconcileSkipReason::NoFindingsNoIssue,
            });
        }
        return ReconcilePlan::new(component_id, actions);
    }

    let body = render_body(component_id, &sections);
    let title = render_title(component_id);
    if let Some(closed) = closed_not_planned {
        if config.refresh_closed_not_planned {
            actions.push(ReconcileAction::UpdateClosed {
                number: closed.number,
                body,
            });
        }
        close_duplicates(&mut actions, &open, closed.number);
        if !config.refresh_closed_not_planned {
            actions.push(ReconcileAction::Skip {
                component_id: component_id.to_string(),
                reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
            });
        }
        return ReconcilePlan::new(component_id, actions);
    }

    if let Some((keep, duplicates)) = open.split_first() {
        actions.push(ReconcileAction::Update {
            number: keep.number,
            title,
            body,
        });
        close_duplicates(&mut actions, duplicates, keep.number);
    } else {
        actions.push(ReconcileAction::FileNew {
            component_id: component_id.to_string(),
            title,
            body,
            labels: vec![ISSUE_LABEL.to_string()],
        });
    }

    ReconcilePlan::new(component_id, actions)
}

fn close_duplicates(actions: &mut Vec<ReconcileAction>, duplicates: &[&TrackedIssue], keep: u64) {
    for issue in duplicates {
        if issue.number == keep {
            continue;
        }
        actions.push(ReconcileAction::CloseDuplicate {
            number: issue.number,
            keep,
            comment: format!(
                "Closing as duplicate of #{keep}. Homeboy now maintains one rolling findings issue per component."
            ),
        });
    }
}

fn collect_sections(canonical: &[&TrackedIssue]) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();

    for issue in canonical {
        if issue.state.is_open() || issue.state == TrackedIssueState::ClosedNotPlanned {
            sections.extend(parse_sections(&issue.body));
        }
    }

    sections
}

fn merge_measurement(
    sections: &mut BTreeMap<String, String>,
    groups: &[IssueGroup],
    command: &str,
    complete_measurement: bool,
) {
    let command_prefix = format!("{command}:");
    let measured: Vec<String> = groups
        .iter()
        .map(|group| section_key(command, &group.category))
        .collect();

    if complete_measurement {
        sections.retain(|key, _| !key.starts_with(&command_prefix) || measured.contains(key));
    }

    for group in groups {
        let key = section_key(command, &group.category);
        if group.count == 0 {
            sections.remove(&key);
        } else {
            sections.insert(key, render_group(group));
        }
    }
}

fn render_group(group: &IssueGroup) -> String {
    let label = if group.label.is_empty() {
        group.category.replace('_', " ")
    } else {
        group.label.clone()
    };
    let mut body = group.body.trim().to_string();
    if body
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("## "))
    {
        body = body.lines().skip(1).collect::<Vec<_>>().join("\n");
    }
    format!(
        "## {}: {}\n\n{}",
        title_case(&group.command),
        label,
        body.trim()
    )
    .trim_end()
    .to_string()
}

fn render_body(component_id: &str, sections: &BTreeMap<String, String>) -> String {
    let mut body = format!(
        "<!-- {}{} -->\n\n# Homeboy findings for `{}`\n\nThis issue is updated automatically from lint, audit, and test runs.\n",
        FINDINGS_KEY_PREFIX, component_id, component_id
    );
    for (key, section) in sections {
        body.push_str(&format!(
            "\n<!-- homeboy:findings-section={key}:start -->\n{}\n<!-- homeboy:findings-section={key}:end -->\n",
            section.trim()
        ));
    }
    body
}

fn parse_sections(body: &str) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();
    let lines: Vec<&str> = body.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let Some(key) = lines[index]
            .strip_prefix(SECTION_PREFIX)
            .and_then(|line| line.strip_suffix(":start -->"))
        else {
            index += 1;
            continue;
        };
        let end = format!("{SECTION_PREFIX}{key}:end -->");
        let start = index + 1;
        index = start;
        while index < lines.len() && lines[index] != end {
            index += 1;
        }
        if index < lines.len() {
            sections.insert(key.to_string(), lines[start..index].join("\n"));
        }
        index += 1;
    }
    sections
}

fn parse_canonical_component(body: &str) -> Option<String> {
    let component = body
        .lines()
        .find_map(|line| {
            line.strip_prefix("<!-- ")?
                .strip_prefix(FINDINGS_KEY_PREFIX)?
                .strip_suffix(" -->")
        })?
        .trim();
    (!component.is_empty()).then(|| component.to_string())
}

fn section_key(command: &str, category: &str) -> String {
    format!("{command}:{category}")
}

fn render_title(component_id: &str) -> String {
    format!("Homeboy findings in {component_id}")
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(command: &str, category: &str, count: usize) -> IssueGroup {
        IssueGroup {
            command: command.into(),
            component_id: "sample-plugin".into(),
            category: category.into(),
            count,
            label: category.replace('_', " "),
            body: format!("## {}\n\n{count} finding(s).", category.replace('_', " ")),
            confidence: None,
        }
    }

    fn tracked(number: u64, title: &str, body: &str, state: TrackedIssueState) -> TrackedIssue {
        TrackedIssue {
            number,
            title: title.into(),
            body: body.into(),
            url: format!("https://example.test/issues/{number}"),
            state,
        }
    }

    fn config() -> ReconcileConfig {
        ReconcileConfig::default()
    }

    #[test]
    fn files_one_aggregate_issue_for_multiple_categories() {
        let groups = vec![group("lint", "formatting", 9), group("lint", "other", 13)];
        let plan = reconcile_measured(&groups, &[], &config(), "lint", "sample-plugin", true);

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::FileNew {
                title,
                body,
                labels,
                ..
            } => {
                assert_eq!(title, "Homeboy findings in sample-plugin");
                assert_eq!(labels, &["homeboy-findings"]);
                assert!(body.contains("findings:sample-plugin"));
                assert!(body.contains("findings-section=lint:formatting:start"));
                assert!(body.contains("findings-section=lint:other:start"));
            }
            action => panic!("expected file_new, got {action:?}"),
        }
    }

    #[test]
    fn command_run_preserves_other_command_sections() {
        let existing_body = render_body(
            "sample-plugin",
            &BTreeMap::from([
                (
                    "audit:structural".into(),
                    "## Audit: structural\n\nold audit".into(),
                ),
                (
                    "lint:formatting".into(),
                    "## Lint: formatting\n\nold lint".into(),
                ),
                (
                    "test:test_failure".into(),
                    "## Test: test failure\n\nold test".into(),
                ),
            ]),
        );
        let existing = tracked(
            10,
            "Homeboy findings in sample-plugin",
            &existing_body,
            TrackedIssueState::Open,
        );

        let plan = reconcile_measured(
            &[group("lint", "formatting", 2)],
            &[existing],
            &config(),
            "lint",
            "sample-plugin",
            true,
        );

        match &plan.actions[0] {
            ReconcileAction::Update { number, body, .. } => {
                assert_eq!(*number, 10);
                assert!(body.contains("old audit"));
                assert!(body.contains("old test"));
                assert!(!body.contains("old lint"));
                assert!(body.contains("2 finding(s)"));
            }
            action => panic!("expected update, got {action:?}"),
        }
    }

    #[test]
    fn complete_measurement_retires_absent_categories_but_narrowed_preserves_them() {
        let existing_body = render_body(
            "sample-plugin",
            &BTreeMap::from([
                ("audit:structural".into(), "structural".into()),
                ("audit:source_policy".into(), "source policy".into()),
            ]),
        );
        let existing = tracked(
            10,
            "Homeboy findings in sample-plugin",
            &existing_body,
            TrackedIssueState::Open,
        );

        let narrowed = reconcile_measured(
            &[group("audit", "structural", 1)],
            &[existing.clone()],
            &config(),
            "audit",
            "sample-plugin",
            false,
        );
        let complete = reconcile_measured(
            &[group("audit", "structural", 1)],
            &[existing],
            &config(),
            "audit",
            "sample-plugin",
            true,
        );

        let body = match &narrowed.actions[0] {
            ReconcileAction::Update { body, .. } => body,
            action => panic!("expected update, got {action:?}"),
        };
        assert!(body.contains("source policy"));
        let body = match &complete.actions[0] {
            ReconcileAction::Update { body, .. } => body,
            action => panic!("expected update, got {action:?}"),
        };
        assert!(!body.contains("source policy"));
    }

    #[test]
    fn closes_only_after_every_command_section_is_clear() {
        let body = render_body(
            "sample-plugin",
            &BTreeMap::from([
                ("lint:formatting".into(), "lint".into()),
                ("test:test_failure".into(), "test".into()),
            ]),
        );
        let existing = tracked(
            10,
            "Homeboy findings in sample-plugin",
            &body,
            TrackedIssueState::Open,
        );

        let lint_clear = reconcile_measured(
            &[],
            &[existing.clone()],
            &config(),
            "lint",
            "sample-plugin",
            true,
        );
        assert!(matches!(
            lint_clear.actions[0],
            ReconcileAction::Update { .. }
        ));

        let lint_cleared_body = match &lint_clear.actions[0] {
            ReconcileAction::Update { body, .. } => body.clone(),
            _ => unreachable!(),
        };
        let after_lint = tracked(
            10,
            "Homeboy findings in sample-plugin",
            &lint_cleared_body,
            TrackedIssueState::Open,
        );
        let test_clear =
            reconcile_measured(&[], &[after_lint], &config(), "test", "sample-plugin", true);
        assert!(matches!(
            test_clear.actions[0],
            ReconcileAction::Close { number: 10, .. }
        ));
    }

    #[test]
    fn unlabeled_canonical_issue_updates_in_place() {
        let body = render_body(
            "sample-plugin",
            &BTreeMap::from([("lint:formatting".into(), "old".into())]),
        );
        let existing = tracked(
            44,
            "Homeboy findings in sample-plugin",
            &body,
            TrackedIssueState::Open,
        );

        let plan = reconcile_measured(
            &[group("lint", "formatting", 3)],
            &[existing],
            &config(),
            "lint",
            "sample-plugin",
            true,
        );
        assert!(matches!(
            plan.actions[0],
            ReconcileAction::Update { number: 44, .. }
        ));
    }

    #[test]
    fn reconciliation_identity_comes_from_canonical_markers_not_titles() {
        let issues = vec![
            tracked(
                30,
                "lint: formatting in sample-plugin (9)",
                "human-owned lint discussion",
                TrackedIssueState::Open,
            ),
            tracked(
                20,
                "Homeboy findings in sample-plugin",
                &render_body(
                    "another-component",
                    &BTreeMap::from([("audit:structural".into(), "other audit".into())]),
                ),
                TrackedIssueState::Open,
            ),
        ];

        let plan = reconcile_measured(
            &[group("lint", "formatting", 1)],
            &issues,
            &config(),
            "lint",
            "sample-plugin",
            true,
        );

        assert_eq!(plan.actions.len(), 1);
        let body = match &plan.actions[0] {
            ReconcileAction::FileNew { body, .. } => body,
            action => panic!("foreign issues must not be mutated: {action:?}"),
        };
        assert!(!body.contains("other audit"));
        assert!(!body.contains("human-owned"));
        assert!(body.contains("1 finding(s)"));
        let empty = reconcile_measured(&[], &issues, &config(), "lint", "sample-plugin", true);
        assert!(matches!(
            empty.actions.as_slice(),
            [ReconcileAction::Skip { .. }]
        ));
    }

    #[test]
    fn canonical_not_planned_issue_remains_closed_and_controls_duplicates() {
        let body = render_body(
            "sample-plugin",
            &BTreeMap::from([("audit:structural".into(), "retained audit".into())]),
        );
        let existing = [
            tracked(
                8,
                "renamed by human",
                &body,
                TrackedIssueState::ClosedNotPlanned,
            ),
            tracked(10, "duplicate", &body, TrackedIssueState::Open),
        ];
        let refreshed = reconcile_measured(
            &[group("lint", "formatting", 2)],
            &existing,
            &config(),
            "lint",
            "sample-plugin",
            true,
        );
        assert!(
            matches!(&refreshed.actions[0], ReconcileAction::UpdateClosed { number: 8, body }
            if body.contains("retained audit") && body.contains("2 finding(s)"))
        );
        assert!(matches!(
            refreshed.actions[1],
            ReconcileAction::CloseDuplicate {
                number: 10,
                keep: 8,
                ..
            }
        ));
        let skipped = reconcile_measured(
            &[group("lint", "formatting", 2)],
            &existing,
            &ReconcileConfig {
                refresh_closed_not_planned: false,
            },
            "lint",
            "sample-plugin",
            true,
        );
        assert!(matches!(
            skipped.actions.as_slice(),
            [
                ReconcileAction::CloseDuplicate {
                    number: 10,
                    keep: 8,
                    ..
                },
                ReconcileAction::Skip {
                    reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn duplicate_canonical_issues_converge_to_lowest_number() {
        let body = render_body(
            "sample-plugin",
            &BTreeMap::from([("test:test_failure".into(), "failed".into())]),
        );
        let issues = vec![
            tracked(12, "Homeboy findings", &body, TrackedIssueState::Open),
            tracked(9, "Homeboy findings", &body, TrackedIssueState::Open),
        ];

        let plan = reconcile_measured(
            &[group("test", "test_failure", 1)],
            &issues,
            &config(),
            "test",
            "sample-plugin",
            true,
        );
        assert!(matches!(
            plan.actions[0],
            ReconcileAction::Update { number: 9, .. }
        ));
        assert!(matches!(
            plan.actions[1],
            ReconcileAction::CloseDuplicate {
                number: 12,
                keep: 9,
                ..
            }
        ));
    }

    #[test]
    fn closed_completed_issue_does_not_revive_stale_sections() {
        let old_body = render_body(
            "sample-plugin",
            &BTreeMap::from([("audit:structural".into(), "resolved audit".into())]),
        );
        let closed = tracked(
            8,
            "Homeboy findings in sample-plugin",
            &old_body,
            TrackedIssueState::ClosedCompleted,
        );

        let plan = reconcile_measured(
            &[group("lint", "formatting", 1)],
            &[closed],
            &config(),
            "lint",
            "sample-plugin",
            true,
        );

        let body = match &plan.actions[0] {
            ReconcileAction::FileNew { body, .. } => body,
            action => panic!("expected file_new, got {action:?}"),
        };
        assert!(!body.contains("resolved audit"));
        assert!(body.contains("findings-section=lint:formatting:start"));
    }
}
