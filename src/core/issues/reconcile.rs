//! The pure reconcile decision function.
//!
//! See homeboy issue #1551 for the full behavior contract. The decision
//! table is encoded in [`reconcile_group`]; [`reconcile`] applies it across
//! every input group and gathers the resulting actions into a single plan.

use std::collections::BTreeMap;

use super::plan::{
    IssueGroup, IssueGroupRouting, ReconcileAction, ReconcileConfig, ReconcilePlan,
    ReconcileSkipReason, TrackedIssue, TrackedIssueState,
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
    // Index existing issues by (command, component, category). The category
    // key is parsed from the title shape `<command>: <label> in <component>`
    // — this matches the convention `auto-file-categorized-issues.sh` has
    // been writing for ~year. Future trackers may store the category in a
    // structured field instead.
    let mut by_category: BTreeMap<(String, String, String), Vec<&TrackedIssue>> = BTreeMap::new();
    for issue in existing {
        if let Some(key) = parse_category_key(&issue.title) {
            by_category.entry(key).or_default().push(issue);
        }
    }

    let mut actions: Vec<ReconcileAction> = Vec::new();

    for group in groups {
        // Phase 1: suppression by config (highest precedence — short-circuits
        // every other consideration).
        if config
            .suppressed_categories
            .iter()
            .any(|c| c == &group.category)
        {
            actions.push(ReconcileAction::Skip {
                category: group.category.clone(),
                component_id: group.component_id.clone(),
                reason: ReconcileSkipReason::SuppressedByConfig,
            });
            continue;
        }

        let key = (
            group.command.clone(),
            group.component_id.clone(),
            group.category.clone(),
        );
        let matches = by_category.get(&key).cloned().unwrap_or_default();

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
        if group.routing == IssueGroupRouting::ReviewOnly {
            actions.push(ReconcileAction::Skip {
                category: group.category.clone(),
                component_id: group.component_id.clone(),
                reason: ReconcileSkipReason::ReviewOnlyDefault,
            });
            continue;
        }

        if !open_matches.is_empty() {
            // Update the lowest-numbered open match.
            let keep = open_matches[0].number;
            actions.push(ReconcileAction::Update {
                number: keep,
                title: render_title(group),
                body: group.body.clone(),
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

        // No open match. Check the closed issues for suppression / refresh.
        if let Some(closed) = preferred_closed {
            match closed.state {
                TrackedIssueState::ClosedNotPlanned => {
                    let has_suppression_label = closed
                        .labels
                        .iter()
                        .any(|l| config.suppression_labels.iter().any(|s| s == l));
                    if has_suppression_label {
                        // Phase 3 suppression — a label on a closed-not_planned
                        // issue mutes re-filing.
                        actions.push(ReconcileAction::Skip {
                            category: group.category.clone(),
                            component_id: group.component_id.clone(),
                            reason: ReconcileSkipReason::SuppressedByLabel,
                        });
                        continue;
                    }
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
                        body: group.body.clone(),
                        category: group.category.clone(),
                        count: group.count,
                    });
                    continue;
                }
                TrackedIssueState::ClosedCompleted => {
                    // Resolved-then-returned: file a fresh issue.
                    actions.push(ReconcileAction::FileNew {
                        command: group.command.clone(),
                        component_id: group.component_id.clone(),
                        category: group.category.clone(),
                        title: render_title(group),
                        body: group.body.clone(),
                        labels: vec![group.command.clone()],
                        count: group.count,
                    });
                    continue;
                }
                TrackedIssueState::Open => unreachable!("partitioned above"),
            }
        }

        // No issue ever existed (open or closed) for this category.
        actions.push(ReconcileAction::FileNew {
            command: group.command.clone(),
            component_id: group.component_id.clone(),
            category: group.category.clone(),
            title: render_title(group),
            body: group.body.clone(),
            labels: vec![group.command.clone()],
            count: group.count,
        });
    }

    ReconcilePlan { actions }
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
// Tests — the 8-row behavior table + suppression precedence
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn group(category: &str, count: usize) -> IssueGroup {
        IssueGroup {
            command: "audit".into(),
            component_id: "data-machine".into(),
            category: category.into(),
            count,
            label: String::new(),
            body: format!("count={}", count),
            routing: IssueGroupRouting::Actionable,
        }
    }

    fn review_only_group(category: &str, count: usize) -> IssueGroup {
        IssueGroup {
            routing: IssueGroupRouting::ReviewOnly,
            ..group(category, count)
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
            title: format!("audit: {} in data-machine ({})", category_label, count),
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
            suppressed_categories: vec![],
            suppression_labels: vec!["wontfix".into(), "upstream-bug".into()],
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
                assert_eq!(title, "audit: unreferenced export in data-machine (12)");
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
                assert_eq!(title, "audit: god file in data-machine (23)");
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
    fn row6_closed_not_planned_with_suppression_label_skips() {
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
            ReconcileAction::Skip { reason, .. } => {
                assert_eq!(*reason, ReconcileSkipReason::SuppressedByLabel);
            }
            other => panic!("expected Skip(SuppressedByLabel), got {:?}", other),
        }
    }

    // --------------------------------------------------------------- ROW 7

    #[test]
    fn row7_suppressed_categories_in_config_skips() {
        let mut config = cfg();
        config.suppressed_categories = vec!["god_file".into()];
        let groups = vec![group("god_file", 99)];
        // Even with an open issue present, config-level suppression wins.
        let existing = vec![issue(675, "god file", TrackedIssueState::Open, 17)];
        let plan = reconcile(&groups, &existing, &config);
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Skip { reason, .. } => {
                assert_eq!(*reason, ReconcileSkipReason::SuppressedByConfig);
            }
            other => panic!("expected Skip(SuppressedByConfig), got {:?}", other),
        }
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

    #[test]
    fn review_only_group_with_findings_skips_issue_actions() {
        let groups = vec![review_only_group("repeated_field_pattern", 18)];
        let existing = vec![issue(
            1622,
            "repeated field pattern",
            TrackedIssueState::Open,
            18,
        )];

        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ReconcileAction::Skip {
                reason, category, ..
            } => {
                assert_eq!(*reason, ReconcileSkipReason::ReviewOnlyDefault);
                assert_eq!(category, "repeated_field_pattern");
            }
            other => panic!("expected review-only Skip, got {:?}", other),
        }
    }

    #[test]
    fn review_only_group_with_zero_findings_still_closes_existing_open_issue() {
        let groups = vec![review_only_group("repeated_field_pattern", 0)];
        let existing = vec![issue(
            1622,
            "repeated field pattern",
            TrackedIssueState::Open,
            18,
        )];

        let plan = reconcile(&groups, &existing, &cfg());
        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Close {
                number: 1622,
                category,
                ..
            } if category == "repeated_field_pattern"
        ));
    }

    // ----------------------------------------------- precedence ladder

    #[test]
    fn precedence_config_beats_open_issue() {
        // Already covered by row7, but keeps the precedence story explicit.
        let mut config = cfg();
        config.suppressed_categories = vec!["x".into()];
        let groups = vec![group("x", 5)];
        let existing = vec![issue(1, "x", TrackedIssueState::Open, 5)];
        let plan = reconcile(&groups, &existing, &config);
        assert!(matches!(
            &plan.actions[0],
            ReconcileAction::Skip {
                reason: ReconcileSkipReason::SuppressedByConfig,
                ..
            }
        ));
    }

    #[test]
    fn precedence_label_only_applies_when_closed_not_planned() {
        // A `wontfix` label on an OPEN issue should NOT suppress — the
        // label-precedence rule is gated on closed-not_planned per #1551
        // (option 2). Open + label = update normally.
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
    fn parse_category_key_round_trips() {
        let title = "audit: unreferenced export in data-machine (57)";
        let (cmd, comp, cat) = parse_category_key(title).unwrap();
        assert_eq!(cmd, "audit");
        assert_eq!(comp, "data-machine");
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
    fn label_falls_back_to_category_with_underscores_replaced() {
        let groups = vec![IssueGroup {
            command: "audit".into(),
            component_id: "x".into(),
            category: "snake_case_thing".into(),
            count: 1,
            label: String::new(),
            body: String::new(),
            routing: IssueGroupRouting::Actionable,
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
            routing: IssueGroupRouting::Actionable,
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
        let plan = ReconcilePlan {
            actions: vec![
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
        };
        let c = plan.counts();
        assert_eq!(c.file_new, 1);
        assert_eq!(c.update, 2);
        assert_eq!(c.skip, 1);
        assert!(!plan.is_noop());
    }
}
