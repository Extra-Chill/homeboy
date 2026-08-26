//! Pure reconciliation for one rolling findings issue per component.

use std::collections::BTreeMap;

use super::plan::{
    IssueGroup, ReconcileAction, ReconcileConfig, ReconcilePlan, ReconcileSkipReason, TrackedIssue,
    TrackedIssueState,
};

const ISSUE_KEY_PREFIX: &str = "<!-- homeboy:issues-reconcile-key=findings:";
const LEGACY_KEY_PREFIX: &str = "<!-- homeboy:issues-reconcile-key=";
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
    let mut related: Vec<&TrackedIssue> = existing
        .iter()
        .filter(|issue| issue_component(issue).as_deref() == Some(component_id))
        .collect();
    related.sort_by_key(|issue| issue.number);

    let canonical: Vec<&TrackedIssue> = related
        .iter()
        .copied()
        .filter(|issue| parse_canonical_component(&issue.body).as_deref() == Some(component_id))
        .collect();
    let mut sections = collect_sections(&canonical, &related, component_id);
    merge_measurement(&mut sections, groups, command, complete_measurement);

    let open: Vec<&TrackedIssue> = related
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
                category: "findings".to_string(),
                comment: "All Homeboy findings have been resolved. Closing automatically."
                    .to_string(),
            });
            close_duplicates(&mut actions, duplicates, keep.number);
        } else {
            actions.push(ReconcileAction::Skip {
                category: "findings".to_string(),
                component_id: component_id.to_string(),
                reason: ReconcileSkipReason::NoFindingsNoIssue,
            });
        }
        return ReconcilePlan::new(component_id, actions);
    }

    let body = render_body(component_id, &sections);
    let title = render_title(component_id);
    let count = groups.iter().map(|group| group.count).sum();

    if let Some(closed) = closed_not_planned {
        if config.refresh_closed_not_planned {
            actions.push(ReconcileAction::UpdateClosed {
                number: closed.number,
                body,
                category: "findings".to_string(),
                count,
            });
        }
        close_duplicates(&mut actions, &open, closed.number);
        if !config.refresh_closed_not_planned {
            actions.push(ReconcileAction::Skip {
                category: "findings".to_string(),
                component_id: component_id.to_string(),
                reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
            });
        }
        return ReconcilePlan::new(component_id, actions);
    }

    if let Some(keep) = preferred_open(&open) {
        actions.push(ReconcileAction::Update {
            number: keep.number,
            title,
            body,
            category: "findings".to_string(),
            count,
        });
        let duplicates: Vec<&TrackedIssue> = open
            .iter()
            .copied()
            .filter(|issue| issue.number != keep.number)
            .collect();
        close_duplicates(&mut actions, &duplicates, keep.number);
    } else {
        actions.push(ReconcileAction::FileNew {
            command: "findings".to_string(),
            component_id: component_id.to_string(),
            category: "findings".to_string(),
            title,
            body,
            labels: vec![ISSUE_LABEL.to_string()],
            count,
        });
    }

    ReconcilePlan::new(component_id, actions)
}

fn preferred_open<'a>(open: &[&'a TrackedIssue]) -> Option<&'a TrackedIssue> {
    open.iter().copied().min_by_key(|issue| {
        (
            parse_canonical_component(&issue.body).is_none(),
            issue.number,
        )
    })
}

fn close_duplicates(actions: &mut Vec<ReconcileAction>, duplicates: &[&TrackedIssue], keep: u64) {
    for issue in duplicates {
        if issue.number == keep {
            continue;
        }
        actions.push(ReconcileAction::CloseDuplicate {
            number: issue.number,
            keep,
            category: "findings".to_string(),
            comment: format!(
                "Closing as duplicate of #{keep}. Homeboy now maintains one rolling findings issue per component."
            ),
        });
    }
}

fn collect_sections(
    canonical: &[&TrackedIssue],
    related: &[&TrackedIssue],
    component_id: &str,
) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();

    for issue in canonical {
        if issue.state.is_open() || issue.state == TrackedIssueState::ClosedNotPlanned {
            sections.extend(parse_sections(&issue.body));
        }
    }

    // Migrate open category-level issues into the rolling document. The next
    // plan deterministically keeps one issue and closes the rest as duplicates.
    for issue in related
        .iter()
        .copied()
        .filter(|issue| issue.state.is_open())
    {
        if parse_canonical_component(&issue.body).is_some() {
            continue;
        }
        if let Some((command, component, category)) =
            parse_legacy_key(&issue.body).or_else(|| parse_legacy_title(&issue.title))
        {
            if component == component_id {
                sections
                    .entry(section_key(&command, &category))
                    .or_insert_with(|| strip_legacy_key(&issue.body));
            }
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
    let mut body = strip_legacy_key(&group.body);
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
        "{}{} -->\n\n# Homeboy findings for `{}`\n\nThis issue is updated automatically from lint, audit, and test runs.\n",
        ISSUE_KEY_PREFIX, component_id, component_id
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

fn issue_component(issue: &TrackedIssue) -> Option<String> {
    parse_canonical_component(&issue.body)
        .or_else(|| parse_legacy_key(&issue.body).map(|(_, component, _)| component))
        .or_else(|| parse_legacy_title(&issue.title).map(|(_, component, _)| component))
}

fn parse_canonical_component(body: &str) -> Option<String> {
    let start = body.find(ISSUE_KEY_PREFIX)? + ISSUE_KEY_PREFIX.len();
    let component = body[start..].split_once(" -->")?.0.trim();
    (!component.is_empty()).then(|| component.to_string())
}

fn parse_legacy_key(body: &str) -> Option<(String, String, String)> {
    let start = body.find(LEGACY_KEY_PREFIX)? + LEGACY_KEY_PREFIX.len();
    let key = body[start..].split_once(" -->")?.0;
    if key.starts_with("findings:") {
        return None;
    }
    let mut parts = key.splitn(3, ':');
    let command = parts.next()?.trim();
    let component = parts.next()?.trim();
    let category = parts.next()?.trim();
    if command.is_empty() || component.is_empty() || category.is_empty() {
        return None;
    }
    Some((command.into(), component.into(), category.into()))
}

fn parse_legacy_title(title: &str) -> Option<(String, String, String)> {
    let (command, rest) = title.split_once(':')?;
    let rest = rest.trim();
    let rest = match rest.rfind(" (") {
        Some(index) if rest.ends_with(')') => &rest[..index],
        _ => rest,
    };
    let index = rest.rfind(" in ")?;
    let label = rest[..index].trim();
    let component = rest[index + 4..].trim();
    if command.is_empty() || label.is_empty() || component.is_empty() {
        return None;
    }
    Some((
        command.trim().to_string(),
        component.to_string(),
        label.replace(' ', "_"),
    ))
}

fn strip_legacy_key(body: &str) -> String {
    body.lines()
        .filter(|line| !line.starts_with(LEGACY_KEY_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
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
            labels: Vec::new(),
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
    fn migrates_legacy_category_issues_and_dedupes_them() {
        let lint = tracked(
            30,
            "lint: formatting in sample-plugin (9)",
            "<!-- homeboy:issues-reconcile-key=lint:sample-plugin:formatting -->\n\nold lint",
            TrackedIssueState::Open,
        );
        let audit = tracked(
            20,
            "audit: structural in sample-plugin (2)",
            "<!-- homeboy:issues-reconcile-key=audit:sample-plugin:structural -->\n\nold audit",
            TrackedIssueState::Open,
        );

        let plan = reconcile_measured(
            &[group("lint", "formatting", 1)],
            &[lint, audit],
            &config(),
            "lint",
            "sample-plugin",
            true,
        );

        assert!(matches!(
            plan.actions[0],
            ReconcileAction::Update { number: 20, .. }
        ));
        assert!(matches!(
            plan.actions[1],
            ReconcileAction::CloseDuplicate {
                number: 30,
                keep: 20,
                ..
            }
        ));
        let body = match &plan.actions[0] {
            ReconcileAction::Update { body, .. } => body,
            _ => unreachable!(),
        };
        assert!(body.contains("old audit"));
        assert!(body.contains("1 finding(s)"));
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
