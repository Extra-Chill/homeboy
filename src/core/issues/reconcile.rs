//! The pure reconcile decision function.
//!
//! See homeboy issue #1551 for the full behavior contract. The decision
//! table is encoded in [`reconcile_group`]; [`reconcile`] applies it across
//! every input group and gathers the resulting actions into a single plan.

use std::collections::BTreeMap;

use super::plan::{
    IssueGroup, ReconcileAction, ReconcileConfig, ReconcilePlan, ReconcileSkipReason, TrackedIssue,
    TrackedIssueState,
};

/// Run the 8-row behavior contract over every group.
///
/// `groups` is the structured finding stream — one row per
/// `(command, component, category)` tuple. `existing` is the tracker's
/// matching issues for the same component+command label scope (caller
/// fetches them with `state=all` so closed-not_planned is visible).
///
/// Pure: no I/O, no clock, no randomness. Same inputs → same plan.
pub fn reconcile(
    groups: &[IssueGroup],
    existing: &[TrackedIssue],
    config: &ReconcileConfig,
) -> ReconcilePlan {
    reconcile_with_scope(groups, existing, config, None)
}

/// Reconcile within an explicit `(command, component)` scope.
///
/// The plain [`reconcile`] function can only act on categories present in the
/// incoming group list. CI action runs also need to converge stale open issues
/// whose category disappeared from the latest command output, including the
/// all-green case where the group list is empty. The explicit scope tells the
/// reconciler which existing tracker issues belong to the current command run.
pub fn reconcile_scoped(
    groups: &[IssueGroup],
    existing: &[TrackedIssue],
    config: &ReconcileConfig,
    command: &str,
    component_id: &str,
) -> ReconcilePlan {
    reconcile_with_scope(groups, existing, config, Some((command, component_id)))
}

fn reconcile_with_scope(
    groups: &[IssueGroup],
    existing: &[TrackedIssue],
    config: &ReconcileConfig,
    scope: Option<(&str, &str)>,
) -> ReconcilePlan {
    // Index existing issues by (command, component, category). New issues carry
    // a stable hidden body key; legacy action-created issues fall back to the
    // title shape `<command>: <label> in <component>`.
    let mut by_category: BTreeMap<(String, String, String), Vec<&TrackedIssue>> = BTreeMap::new();
    for issue in existing {
        if let Some(key) = parse_issue_key(&issue.body).or_else(|| parse_category_key(&issue.title))
        {
            by_category.entry(key).or_default().push(issue);
        }
    }

    let mut actions: Vec<ReconcileAction> = Vec::new();
    let mut seen_keys: Vec<(String, String, String)> = Vec::new();

    for group in groups {
        let key = (
            group.command.clone(),
            group.component_id.clone(),
            group.category.clone(),
        );
        seen_keys.push(key.clone());
        let matches = collect_matches(&by_category, group, &key);

        // Phase 2: dispatch on (existing-issue-shape, count).
        let (open_matches, closed_matches): (Vec<_>, Vec<_>) =
            matches.into_iter().partition(|i| i.state.is_open());

        // Sort opens by issue number so dedupe is deterministic (lowest kept).
        let mut open_matches = open_matches;
        open_matches.sort_by_key(|i| i.number);

        // Pick a closed issue to consider for state-reason precedence:
        // not_planned beats completed (the muting signal is more interesting
        // than the resolved signal), then most-recent (highest number) wins.
        let preferred_closed = pick_preferred_closed(&closed_matches);

        if group.count == 0 {
            // No findings remaining for this category.
            if let Some((_, rest)) = open_matches.split_first() {
                let keep = open_matches[0].number;
                // Close every open match (no reason to keep one if there are
                // no findings — fold dedupes in for free).
                actions.push(ReconcileAction::Close {
                    number: keep,
                    category: group.category.clone(),
                    comment: close_resolved_comment(&group.label_or_category()),
                });
                for dup in rest {
                    actions.push(ReconcileAction::CloseDuplicate {
                        number: dup.number,
                        keep,
                        category: group.category.clone(),
                        comment: close_dedupe_comment(keep),
                    });
                }
            } else {
                // Nothing to do — no findings, no existing issue.
                actions.push(ReconcileAction::Skip {
                    category: group.category.clone(),
                    component_id: group.component_id.clone(),
                    reason: ReconcileSkipReason::NoFindingsNoIssue,
                });
            }
            continue;
        }

        // count > 0 from here.
        if let Some(closed) =
            preferred_closed.filter(|issue| issue.state == TrackedIssueState::ClosedNotPlanned)
        {
            // A human closing the category as not_planned is stronger than any
            // open duplicate left behind by an older action run. Keep the
            // closed issue as the canonical record and fold open dupes into it.
            if config.refresh_closed_not_planned {
                actions.push(ReconcileAction::UpdateClosed {
                    number: closed.number,
                    body: body_with_issue_key(group),
                    category: group.category.clone(),
                    count: group.count,
                });
            }
            for dup in &open_matches {
                actions.push(ReconcileAction::CloseDuplicate {
                    number: dup.number,
                    keep: closed.number,
                    category: group.category.clone(),
                    comment: close_dedupe_comment(closed.number),
                });
            }
            if !config.refresh_closed_not_planned {
                actions.push(ReconcileAction::Skip {
                    category: group.category.clone(),
                    component_id: group.component_id.clone(),
                    reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
                });
            }
            continue;
        }

        if !open_matches.is_empty() {
            // Update the lowest-numbered open match.
            let keep = open_matches[0].number;
            actions.push(ReconcileAction::Update {
                number: keep,
                title: render_title(group),
                body: body_with_issue_key(group),
                category: group.category.clone(),
                count: group.count,
            });
            // Close any other open dupes (race-condition consolidation).
            for dup in &open_matches[1..] {
                actions.push(ReconcileAction::CloseDuplicate {
                    number: dup.number,
                    keep,
                    category: group.category.clone(),
                    comment: close_dedupe_comment(keep),
                });
            }
            continue;
        }

        // No open match. Check closed issue history before filing a fresh one.
        if let Some(closed) = preferred_closed {
            match closed.state {
                TrackedIssueState::ClosedNotPlanned => {
                    if !config.refresh_closed_not_planned {
                        actions.push(ReconcileAction::Skip {
                            category: group.category.clone(),
                            component_id: group.component_id.clone(),
                            reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
                        });
                        continue;
                    }
                    actions.push(ReconcileAction::UpdateClosed {
                        number: closed.number,
                        body: body_with_issue_key(group),
                        category: group.category.clone(),
                        count: group.count,
                    });
                    continue;
                }
                TrackedIssueState::ClosedCompleted => {
                    // Resolved-then-returned: file a fresh issue.
                    push_file_new(&mut actions, group);
                    continue;
                }
                TrackedIssueState::Open => unreachable!("partitioned above"),
            }
        }

        // No issue ever existed (open or closed) for this category.
        push_file_new(&mut actions, group);
    }

    if let Some((command, component_id)) = scope {
        close_absent_open_issues(
            &mut actions,
            &by_category,
            &seen_keys,
            command,
            component_id,
        );
    }

    let component_id = scope
        .map(|(_, component_id)| component_id.to_string())
        .or_else(|| groups.first().map(|group| group.component_id.clone()))
        .or_else(|| {
            existing.iter().find_map(|issue| {
                parse_issue_key(&issue.body)
                    .or_else(|| parse_category_key(&issue.title))
                    .map(|(_, component_id, _)| component_id)
            })
        })
        .unwrap_or_else(|| "unknown".to_string());

    ReconcilePlan::new(component_id, actions)
}

fn push_file_new(actions: &mut Vec<ReconcileAction>, group: &IssueGroup) {
    actions.push(ReconcileAction::FileNew {
        command: group.command.clone(),
        component_id: group.component_id.clone(),
        category: group.category.clone(),
        title: render_title(group),
        body: body_with_issue_key(group),
        labels: vec![group.command.clone()],
        count: group.count,
    });
}

fn close_absent_open_issues(
    actions: &mut Vec<ReconcileAction>,
    by_category: &BTreeMap<(String, String, String), Vec<&TrackedIssue>>,
    seen_keys: &[(String, String, String)],
    command: &str,
    component_id: &str,
) {
    for ((issue_command, issue_component, category), matches) in by_category {
        if issue_command != command || issue_component != component_id {
            continue;
        }
        if seen_keys.contains(&(
            issue_command.clone(),
            issue_component.clone(),
            category.clone(),
        )) {
            continue;
        }
        let mut open_matches: Vec<_> = matches
            .iter()
            .copied()
            .filter(|issue| issue.state.is_open())
            .collect();
        if open_matches.is_empty() {
            continue;
        }

        open_matches.sort_by_key(|issue| issue.number);
        let keep = open_matches[0].number;
        actions.push(ReconcileAction::Close {
            number: keep,
            category: category.clone(),
            comment: close_resolved_comment(&category.replace('_', " ")),
        });
        for dup in &open_matches[1..] {
            actions.push(ReconcileAction::CloseDuplicate {
                number: dup.number,
                keep,
                category: category.clone(),
                comment: close_dedupe_comment(keep),
            });
        }
    }
}

fn collect_matches<'a>(
    by_category: &BTreeMap<(String, String, String), Vec<&'a TrackedIssue>>,
    group: &IssueGroup,
    key: &(String, String, String),
) -> Vec<&'a TrackedIssue> {
    let mut matches = by_category.get(key).cloned().unwrap_or_default();

    // Legacy action-created issues did not carry the stable body key. If the
    // displayed label does not round-trip to the canonical category (notably
    // aggregate test failures: `_aggregate` -> `test failure (exit 101)`),
    // recognize the old title-derived key once so the update path writes the
    // stable key instead of filing a duplicate.
    let legacy_category = group.label_or_category().replace(' ', "_");
    if legacy_category != group.category {
        let legacy_key = (
            group.command.clone(),
            group.component_id.clone(),
            legacy_category,
        );
        if let Some(legacy_matches) = by_category.get(&legacy_key) {
            let mut seen: Vec<u64> = matches.iter().map(|i| i.number).collect();
            for issue in legacy_matches {
                if !seen.contains(&issue.number) {
                    matches.push(issue);
                    seen.push(issue.number);
                }
            }
        }
    }

    matches
}

fn pick_preferred_closed<'a>(closed: &[&'a TrackedIssue]) -> Option<&'a TrackedIssue> {
    // not_planned beats completed; otherwise highest number (most recent).
    closed
        .iter()
        .copied()
        .max_by_key(|i| (i.state == TrackedIssueState::ClosedNotPlanned, i.number))
}

fn render_title(group: &IssueGroup) -> String {
    format!(
        "{}: {} in {} ({})",
        group.command,
        group.label_or_category(),
        group.component_id,
        group.count
    )
}

const ISSUE_KEY_PREFIX: &str = "<!-- homeboy:issues-reconcile-key=";

fn issue_key(command: &str, component: &str, category: &str) -> String {
    format!("{}:{}:{}", command, component, category)
}

fn issue_key_marker(group: &IssueGroup) -> String {
    format!(
        "{}{} -->",
        ISSUE_KEY_PREFIX,
        issue_key(&group.command, &group.component_id, &group.category)
    )
}

fn body_with_issue_key(group: &IssueGroup) -> String {
    if group.body.contains(ISSUE_KEY_PREFIX) {
        group.body.clone()
    } else if group.body.is_empty() {
        issue_key_marker(group)
    } else {
        format!("{}\n\n{}", issue_key_marker(group), group.body)
    }
}

fn parse_issue_key(body: &str) -> Option<(String, String, String)> {
    let start = body.find(ISSUE_KEY_PREFIX)? + ISSUE_KEY_PREFIX.len();
    let rest = &body[start..];
    let end = rest.find(" -->")?;
    let key = &rest[..end];
    let mut parts = key.splitn(3, ':');
    let command = parts.next()?.trim();
    let component = parts.next()?.trim();
    let category = parts.next()?.trim();
    if command.is_empty() || component.is_empty() || category.is_empty() {
        return None;
    }
    Some((
        command.to_string(),
        component.to_string(),
        category.to_string(),
    ))
}

fn close_resolved_comment(label: &str) -> String {
    format!(
        "All **{}** findings have been resolved. Closing automatically.\n\n\
         Resolved by `homeboy issues reconcile`. If findings reappear, a new \
         issue will be filed.",
        label
    )
}

fn close_dedupe_comment(keep: u64) -> String {
    format!(
        "Closing as duplicate of #{} — consolidated by `homeboy issues reconcile`.\n\n\
         Going forward, a single issue per category is maintained and updated \
         on each CI run.",
        keep
    )
}

/// Parse `<command>: <label> in <component>` (with optional `(N)` suffix)
/// out of an issue title. Returns `(command, component, category_key)`.
///
/// `category_key` is the underscore-form (`unreferenced_export`) reconstructed
/// from the human label (`unreferenced export`). This mirrors the title shape
/// `auto-file-categorized-issues.sh` has been writing.
fn parse_category_key(title: &str) -> Option<(String, String, String)> {
    // Match `command: label in component(... maybe (N) ...)`.
    let colon = title.find(':')?;
    let command = title[..colon].trim().to_string();
    let rest = title[colon + 1..].trim();

    // Strip optional trailing `(N)` count.
    let rest = match rest.rfind(" (") {
        Some(idx) if rest.ends_with(')') => &rest[..idx],
        _ => rest,
    };

    // Split off ` in <component>` from the right.
    let in_idx = rest.rfind(" in ")?;
    let label = rest[..in_idx].trim().to_string();
    let component = rest[in_idx + 4..].trim().to_string();

    if command.is_empty() || label.is_empty() || component.is_empty() {
        return None;
    }
    let category = label.replace(' ', "_");
    Some((command, component, category))
}

impl IssueGroup {
    fn label_or_category(&self) -> String {
        if self.label.is_empty() {
            self.category.replace('_', " ")
        } else {
            self.label.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — the 8-row behavior table and edge cases
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::FindingConfidence;

    fn group(category: &str, count: usize) -> IssueGroup {
        IssueGroup {
            command: "audit".into(),
            component_id: "sample-plugin".into(),
            category: category.into(),
            count,
            label: String::new(),
            body: format!("count={}", count),
            confidence: None,
        }
    }

    fn issue(
        number: u64,
        category_label: &str,
        state: TrackedIssueState,
        count: usize,
    ) -> TrackedIssue {
        TrackedIssue {
            number,
            title: format!("audit: {} in sample-plugin ({})", category_label, count),
            body: String::new(),
            url: format!("https://github.com/o/r/issues/{}", number),
            state,
            labels: vec!["audit".into()],
        }
    }

    fn issue_with_labels(
        number: u64,
        category_label: &str,
        state: TrackedIssueState,
        count: usize,
        labels: &[&str],
    ) -> TrackedIssue {
        let mut iss = issue(number, category_label, state, count);
        iss.labels = labels.iter().map(|s| s.to_string()).collect();
        iss
    }

    fn cfg() -> ReconcileConfig {
        ReconcileConfig {
            refresh_closed_not_planned: true,
        }
    }

    // --------------------------------------------------------------- ROW 1

    #[test]
    fn row1_no_issue_ever_with_findings_files_new() {
        let groups = vec![group("unreferenced_export", 12)];
        let plan = reconcile(&groups, &[], &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::FileNew {
                title,
                count,
                labels,
                ..
            } => {
                assert_eq!(*count, 12);
                assert_eq!(title, "audit: unreferenced export in sample-plugin (12)");
                assert_eq!(labels, &vec!["audit".to_string()]);
            }
            other => panic!("expected FileNew, got {:?}", other),
        }
    }

    // --------------------------------------------------------------- ROW 2

    #[test]
    fn row2_open_issue_with_findings_updates() {
        let groups = vec![group("god_file", 23)];
        let existing = vec![issue(675, "god file", TrackedIssueState::Open, 17)];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Update {
                number,
                count,
                title,
                ..
            } => {
                assert_eq!(*number, 675);
                assert_eq!(*count, 23);
                assert_eq!(title, "audit: god file in sample-plugin (23)");
            }
            other => panic!("expected Update, got {:?}", other),
        }
    }

    // --------------------------------------------------------------- ROW 3

    #[test]
    fn row3_open_issue_zero_findings_closes() {
        let groups = vec![group("legacy_comment", 0)];
        let existing = vec![issue(1449, "legacy comment", TrackedIssueState::Open, 1)];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Close {
                number,
                category,
                comment,
            } => {
                assert_eq!(*number, 1449);
                assert_eq!(category, "legacy_comment");
                assert!(comment.contains("legacy comment"));
                assert!(comment.contains("Resolved"));
            }
            other => panic!("expected Close, got {:?}", other),
        }
    }

    // --------------------------------------------------------------- ROW 4

    #[test]
    fn row4_closed_completed_with_findings_files_new() {
        // The original issue auto-resolved. Findings came back.
        // Same shape as Row 1 but with a closed-completed issue in history.
        let groups = vec![group("unreferenced_export", 5)];
        let existing = vec![issue(
            684,
            "unreferenced export",
            TrackedIssueState::ClosedCompleted,
            0,
        )];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(&plan.actions[0], ReconcileAction::FileNew { count, .. } if *count == 5));
    }

    // --------------------------------------------------------------- ROW 5

    #[test]
    fn row5_closed_not_planned_with_findings_refreshes_body() {
        // Human closed the issue saying "don't bug me about this, it's a
        // false positive." Findings still produced. Refresh body, stay closed.
        let groups = vec![group("missing_method", 164)];
        let existing = vec![issue(
            719,
            "missing method",
            TrackedIssueState::ClosedNotPlanned,
            0,
        )];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::UpdateClosed { number, count, .. } => {
                assert_eq!(*number, 719);
                assert_eq!(*count, 164);
            }
            other => panic!("expected UpdateClosed, got {:?}", other),
        }
    }

    // --------------------------------------------------------------- ROW 6

    #[test]
    fn row6_closed_not_planned_with_labels_still_refreshes_body() {
        let groups = vec![group("missing_test_method", 334)];
        let existing = vec![issue_with_labels(
            802,
            "missing test method",
            TrackedIssueState::ClosedNotPlanned,
            0,
            &["audit", "wontfix"],
        )];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::UpdateClosed { number, count, .. } => {
                assert_eq!(*number, 802);
                assert_eq!(*count, 334);
            }
            other => panic!("expected UpdateClosed, got {:?}", other),
        }
    }

    // --------------------------------------------------------------- ROW 7

    #[test]
    fn row7_open_issue_with_findings_updates_even_for_noisy_categories() {
        let groups = vec![group("god_file", 99)];
        let existing = vec![issue(675, "god file", TrackedIssueState::Open, 17)];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Update { number, count, .. } => {
                assert_eq!(*number, 675);
                assert_eq!(*count, 99);
            }
            other => panic!("expected Update, got {:?}", other),
        }
    }

    #[test]
    fn heuristic_confidence_group_files_issue() {
        let mut heuristic = group("extension_specific_hint", 3);
        heuristic.confidence = Some(FindingConfidence::Heuristic);

        let plan = reconcile(&[heuristic], &[], &cfg());

        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(&plan.actions[0], ReconcileAction::FileNew { count, .. } if *count == 3));
    }

    // --------------------------------------------------------------- ROW 8

    #[test]
    fn row8_multiple_open_for_same_category_dedupes() {
        let groups = vec![group("high_item_count", 52)];
        let existing = vec![
            issue(676, "high item count", TrackedIssueState::Open, 50),
            issue(1253, "high item count", TrackedIssueState::Open, 50),
        ];
        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 2);
        // First action: update the lowest-numbered (676).
        match &plan.actions[0] {
            ReconcileAction::Update { number, .. } => assert_eq!(*number, 676),
            other => panic!("expected Update on #676, got {:?}", other),
        }
        // Second action: close-as-duplicate the higher-numbered (1253).
        match &plan.actions[1] {
            ReconcileAction::CloseDuplicate { number, keep, .. } => {
                assert_eq!(*number, 1253);
                assert_eq!(*keep, 676);
            }
            other => panic!("expected CloseDuplicate, got {:?}", other),
        }
    }

    // ----------------------------------------------- precedence ladder

    #[test]
    fn open_issue_with_findings_updates_for_any_category() {
        let groups = vec![group("x", 5)];
        let existing = vec![issue(1, "x", TrackedIssueState::Open, 5)];
        let plan = reconcile(&groups, &existing, &cfg());
        assert!(matches!(&plan.actions[0], ReconcileAction::Update { .. }));
    }

    #[test]
    fn labels_do_not_mute_open_issue_updates() {
        let groups = vec![group("x", 5)];
        let existing = vec![issue_with_labels(
            1,
            "x",
            TrackedIssueState::Open,
            3,
            &["audit", "wontfix"],
        )];
        let plan = reconcile(&groups, &existing, &cfg());
        assert!(matches!(&plan.actions[0], ReconcileAction::Update { .. }));
    }

    #[test]
    fn precedence_refresh_disabled_for_closed_not_planned_skips() {
        let mut config = cfg();
        config.refresh_closed_not_planned = false;
        let groups = vec![group("x", 5)];
        let existing = vec![issue(1, "x", TrackedIssueState::ClosedNotPlanned, 0)];
        let plan = reconcile(&groups, &existing, &config);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Skip {
                reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
                ..
            }
        ));
    }

    // ---------------------------------------------------- edge cases

    #[test]
    fn no_findings_no_issue_skips_silently() {
        let groups = vec![group("x", 0)];
        let plan = reconcile(&groups, &[], &cfg());
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Skip {
                reason: ReconcileSkipReason::NoFindingsNoIssue,
                ..
            }
        ));
    }

    #[test]
    fn closed_not_planned_beats_closed_completed_when_both_exist() {
        // Both closed in history. not_planned wins (the "do not re-file"
        // signal is more specific than "we resolved it once").
        let groups = vec![group("x", 5)];
        let existing = vec![
            issue(10, "x", TrackedIssueState::ClosedCompleted, 0),
            issue(20, "x", TrackedIssueState::ClosedNotPlanned, 0),
        ];
        let plan = reconcile(&groups, &existing, &cfg());
        match &plan.actions[0] {
            ReconcileAction::UpdateClosed { number, .. } => assert_eq!(*number, 20),
            other => panic!("expected UpdateClosed on #20, got {:?}", other),
        }
    }

    #[test]
    fn closed_not_planned_beats_later_open_duplicate() {
        let groups = vec![group("dead_guard", 23)];
        let existing = vec![
            issue(1364, "dead guard", TrackedIssueState::ClosedNotPlanned, 23),
            issue(1377, "dead guard", TrackedIssueState::Open, 23),
        ];

        let plan = reconcile(&groups, &existing, &cfg());

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::UpdateClosed {
                number: 1364,
                category,
                count: 23,
                ..
            } if category == "dead_guard"
        ));
        assert!(matches!(
            &plan.actions[1],
            ReconcileAction::CloseDuplicate {
                number: 1377,
                keep: 1364,
                category,
                ..
            } if category == "dead_guard"
        ));
    }

    #[test]
    fn closed_not_planned_with_labels_updates_and_closes_later_open_duplicate() {
        let groups = vec![group("unreferenced_export", 1)];
        let existing = vec![
            issue_with_labels(
                1366,
                "unreferenced export",
                TrackedIssueState::ClosedNotPlanned,
                1,
                &["audit", "wontfix"],
            ),
            issue(1400, "unreferenced export", TrackedIssueState::Open, 1),
        ];

        let plan = reconcile(&groups, &existing, &cfg());

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::UpdateClosed { number: 1366, .. }
        ));
        assert!(matches!(
            &plan.actions[1],
            ReconcileAction::CloseDuplicate {
                number: 1400,
                keep: 1366,
                category,
                ..
            } if category == "unreferenced_export"
        ));
    }

    #[test]
    fn parse_category_key_round_trips() {
        let title = "audit: unreferenced export in sample-plugin (57)";
        let (cmd, comp, cat) = parse_category_key(title).unwrap();
        assert_eq!(cmd, "audit");
        assert_eq!(comp, "sample-plugin");
        assert_eq!(cat, "unreferenced_export");
    }

    #[test]
    fn parse_category_key_handles_missing_count_suffix() {
        // Aggregate issues don't always carry `(N)`.
        let title = "test: failures in homeboy";
        let (cmd, comp, cat) = parse_category_key(title).unwrap();
        assert_eq!(cmd, "test");
        assert_eq!(comp, "homeboy");
        assert_eq!(cat, "failures");
    }

    #[test]
    fn issue_body_key_takes_precedence_over_title_label() {
        let mut existing = issue(
            1682,
            "test failure (exit 101)",
            TrackedIssueState::Open,
            101,
        );
        existing.title = "test: test failure (exit 101) in homeboy (101)".into();
        existing.body =
            "<!-- homeboy:issues-reconcile-key=test:homeboy:_aggregate -->\n\nold".into();
        existing.labels = vec!["test".into()];

        let groups = vec![IssueGroup {
            command: "test".into(),
            component_id: "homeboy".into(),
            category: "_aggregate".into(),
            count: 101,
            label: "test failure (exit 101)".into(),
            body: "new body".into(),
            confidence: None,
        }];

        let plan = reconcile(&groups, &[existing], &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Update { number, body, .. } => {
                assert_eq!(*number, 1682);
                assert!(body.contains("homeboy:issues-reconcile-key=test:homeboy:_aggregate"));
                assert!(body.contains("new body"));
            }
            other => panic!("expected Update, got {:?}", other),
        }
    }

    #[test]
    fn legacy_aggregate_title_updates_instead_of_filing_duplicate() {
        let existing = TrackedIssue {
            number: 1676,
            title: "test: test failure (exit 101) in homeboy (101)".into(),
            body: String::new(),
            url: "https://github.com/o/r/issues/1676".into(),
            state: TrackedIssueState::Open,
            labels: vec!["test".into()],
        };
        let groups = vec![IssueGroup {
            command: "test".into(),
            component_id: "homeboy".into(),
            category: "_aggregate".into(),
            count: 101,
            label: "test failure (exit 101)".into(),
            body: "fresh aggregate body".into(),
            confidence: None,
        }];

        let plan = reconcile(&groups, &[existing], &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Update {
                number,
                title,
                body,
                category,
                ..
            } => {
                assert_eq!(*number, 1676);
                assert_eq!(title, "test: test failure (exit 101) in homeboy (101)");
                assert_eq!(category, "_aggregate");
                assert!(body
                    .starts_with("<!-- homeboy:issues-reconcile-key=test:homeboy:_aggregate -->"));
            }
            other => panic!("expected Update, got {:?}", other),
        }
    }

    #[test]
    fn file_new_body_includes_stable_issue_key() {
        let groups = vec![IssueGroup {
            command: "test".into(),
            component_id: "homeboy".into(),
            category: "_aggregate".into(),
            count: 101,
            label: "test failure (exit 101)".into(),
            body: "body".into(),
            confidence: None,
        }];

        let plan = reconcile(&groups, &[], &cfg());
        match &plan.actions[0] {
            ReconcileAction::FileNew { body, .. } => {
                assert!(body
                    .starts_with("<!-- homeboy:issues-reconcile-key=test:homeboy:_aggregate -->"));
                assert!(body.contains("body"));
            }
            other => panic!("expected FileNew, got {:?}", other),
        }
    }

    #[test]
    fn parse_category_key_returns_none_for_garbage() {
        assert!(parse_category_key("not a homeboy issue").is_none());
        assert!(parse_category_key("audit: missing component").is_none());
    }

    #[test]
    fn empty_groups_produces_empty_plan() {
        let plan = reconcile(&[], &[], &cfg());
        assert!(plan.actions.is_empty());
        assert!(plan.is_noop());
    }

    #[test]
    fn scoped_empty_groups_close_all_open_issues_in_scope() {
        let existing = vec![
            issue(10, "dead guard", TrackedIssueState::Open, 4),
            issue(20, "unreferenced export", TrackedIssueState::Open, 9),
            issue(30, "closed thing", TrackedIssueState::ClosedCompleted, 0),
        ];

        let plan = reconcile_scoped(&[], &existing, &cfg(), "audit", "sample-plugin");

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Close {
                number: 10,
                category,
                ..
            } if category == "dead_guard"
        ));
        assert!(matches!(
            &plan.actions[1],
            ReconcileAction::Close {
                number: 20,
                category,
                ..
            } if category == "unreferenced_export"
        ));
    }

    #[test]
    fn scoped_missing_category_closes_stale_open_issue() {
        let groups = vec![group("dead_guard", 3)];
        let existing = vec![
            issue(10, "dead guard", TrackedIssueState::Open, 4),
            issue(20, "unreferenced export", TrackedIssueState::Open, 9),
        ];

        let plan = reconcile_scoped(&groups, &existing, &cfg(), "audit", "sample-plugin");

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Update { number: 10, .. }
        ));
        assert!(matches!(
            &plan.actions[1],
            ReconcileAction::Close {
                number: 20,
                category,
                ..
            } if category == "unreferenced_export"
        ));
    }

    #[test]
    fn scoped_absent_issue_closes_duplicate_open_issues_deterministically() {
        let existing = vec![
            issue(20, "dead guard", TrackedIssueState::Open, 4),
            issue(10, "dead guard", TrackedIssueState::Open, 4),
        ];

        let plan = reconcile_scoped(&[], &existing, &cfg(), "audit", "sample-plugin");

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Close { number: 10, .. }
        ));
        assert!(matches!(
            &plan.actions[1],
            ReconcileAction::CloseDuplicate {
                number: 20,
                keep: 10,
                ..
            }
        ));
    }

    #[test]
    fn scoped_absent_issue_honors_component_scope() {
        let mut other_component = issue(10, "dead guard", TrackedIssueState::Open, 4);
        other_component.title = "audit: dead guard in other-component (4)".into();
        let existing = vec![
            other_component,
            issue(20, "god file", TrackedIssueState::Open, 9),
            issue(30, "unreferenced export", TrackedIssueState::Open, 2),
        ];
        let plan = reconcile_scoped(&[], &existing, &cfg(), "audit", "sample-plugin");

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Close {
                number: 20,
                category,
                ..
            } if category == "god_file"
        ));
        assert!(matches!(
            &plan.actions[1],
            ReconcileAction::Close {
                number: 30,
                category,
                ..
            } if category == "unreferenced_export"
        ));
    }

    #[test]
    fn label_falls_back_to_category_with_underscores_replaced() {
        let groups = vec![IssueGroup {
            command: "audit".into(),
            component_id: "x".into(),
            category: "snake_case_thing".into(),
            count: 1,
            label: String::new(),
            body: String::new(),
            confidence: None,
        }];
        let plan = reconcile(&groups, &[], &cfg());
        match &plan.actions[0] {
            ReconcileAction::FileNew { title, .. } => {
                assert!(title.contains("snake case thing"));
            }
            _ => panic!("expected FileNew"),
        }
    }

    #[test]
    fn explicit_label_used_in_title_when_provided() {
        let groups = vec![IssueGroup {
            command: "lint".into(),
            component_id: "x".into(),
            category: "i18n".into(),
            count: 3,
            label: "i18n / l10n".into(),
            body: String::new(),
            confidence: None,
        }];
        let plan = reconcile(&groups, &[], &cfg());
        match &plan.actions[0] {
            ReconcileAction::FileNew { title, .. } => {
                assert_eq!(title, "lint: i18n / l10n in x (3)");
            }
            _ => panic!("expected FileNew"),
        }
    }

    #[test]
    fn plan_counts_aggregate_correctly() {
        let plan = ReconcilePlan::new(
            "c",
            vec![
                ReconcileAction::FileNew {
                    command: "a".into(),
                    component_id: "c".into(),
                    category: "k".into(),
                    title: "t".into(),
                    body: "b".into(),
                    labels: vec![],
                    count: 1,
                },
                ReconcileAction::Update {
                    number: 1,
                    title: "t".into(),
                    body: "b".into(),
                    category: "k".into(),
                    count: 1,
                },
                ReconcileAction::Update {
                    number: 2,
                    title: "t".into(),
                    body: "b".into(),
                    category: "k".into(),
                    count: 1,
                },
                ReconcileAction::Skip {
                    category: "k".into(),
                    component_id: "c".into(),
                    reason: ReconcileSkipReason::NoFindingsNoIssue,
                },
            ],
        );
        let c = plan.counts();
        assert_eq!(c.file_new, 1);
        assert_eq!(c.update, 2);
        assert_eq!(c.skip, 1);
        assert!(!plan.is_noop());
    }
}
