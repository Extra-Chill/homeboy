//! Cook delivery items derived from the task checklist (#18900).
//!
//! The task source snapshot (#18890) carries the issue or pull request body
//! Homeboy read at admission. Its checklist — the list items under a
//! `Required behaviour` or `Acceptance criteria` heading, and every task-list
//! item — becomes the delivery items the provider is shown and against which
//! the review form's `delivery` is reconciled at finalization. An item the
//! agent did not report becomes `not_delivered`, which publishes the
//! candidate as a draft settling `succeeded_partial` (#18884). A task without
//! a checklist keeps the agent's own accounting unchanged.

use homeboy_control_plane_contract::{CookDeliveryItem, CookDeliveryItemStatus};

use crate::agent_task::AgentTaskRequest;
use crate::agent_task_source_snapshot::{TaskSourceSnapshot, TASK_SOURCE_INPUT_KEY};

/// Reason recorded for a checklist item the review form did not account for.
pub const UNREPORTED_DELIVERY_REASON: &str = "not reported by the agent";

/// Words that mark a heading whose list items are delivery items, e.g.
/// `Required behaviour`, `Acceptance criteria`, `Asks` or
/// `Rework 2 (required): …`.
const CHECKLIST_HEADING_WORDS: [&str; 6] = [
    "required",
    "acceptance criteria",
    "asks",
    "tasks",
    "rework",
    "to do",
];

/// The task source snapshot recorded on `request` at admission.
pub fn request_task_source(request: &AgentTaskRequest) -> Option<TaskSourceSnapshot> {
    let value = request
        .inputs
        .get(crate::agent_task_prompt_template::HOMEBOY_CONTEXT_INPUT_KEY)?
        .get(TASK_SOURCE_INPUT_KEY)?;
    serde_json::from_value(value.clone()).ok()
}

/// The delivery items of the task `request` carries; empty without a
/// recorded snapshot or without a checklist in it.
pub fn request_delivery_items(request: &AgentTaskRequest) -> Vec<String> {
    request_task_source(request)
        .map(|snapshot| snapshot_delivery_items(&snapshot))
        .unwrap_or_default()
}

/// The checklist of the task body, followed by that of the issue a pull
/// request rework was resolved from.
pub fn snapshot_delivery_items(snapshot: &TaskSourceSnapshot) -> Vec<String> {
    let mut items = checklist_items(&snapshot.body);
    if let Some(issue) = snapshot.linked_issue.as_deref() {
        for item in checklist_items(&issue.body) {
            if !items
                .iter()
                .any(|known| normalized(known) == normalized(&item))
            {
                items.push(item);
            }
        }
    }
    items
}

/// Extract the checklist of a Markdown body: list items under a
/// `Required behaviour` / `Acceptance criteria` heading, and task-list items
/// anywhere. Fenced code is skipped; duplicates keep their first position.
pub fn checklist_items(body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = heading_text(trimmed) {
            let heading = heading.to_lowercase();
            in_section = CHECKLIST_HEADING_WORDS
                .iter()
                .any(|word| heading.contains(word));
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let item = match list_item(trimmed) {
            Some(text) => match task_item(text) {
                Some(task) => Some(task),
                None if in_section && indent < 2 => Some(text),
                None => None,
            },
            None => None,
        };
        let Some(item) = item
            .map(|item| single_line(bold_title(item)))
            .filter(|item| !item.is_empty())
        else {
            continue;
        };
        if !items
            .iter()
            .any(|known| normalized(known) == normalized(&item))
        {
            items.push(item);
        }
    }
    items
}

/// Reconcile the agent's reported `delivery` against the task `checklist`.
/// Each checklist item takes the verdict of the reported entry that names it
/// or, unreported, becomes `not_delivered`; reported entries that name no
/// checklist item follow. Without a checklist the report stands as is.
pub fn reconcile_delivery(
    checklist: &[String],
    reported: &[CookDeliveryItem],
) -> Vec<CookDeliveryItem> {
    if checklist.is_empty() {
        return reported.to_vec();
    }
    let mut used = vec![false; reported.len()];
    let mut find = |item: &str, matches: &dyn Fn(&str, &str) -> bool| {
        let index = reported
            .iter()
            .enumerate()
            .position(|(index, entry)| !used[index] && matches(item, &entry.item))?;
        used[index] = true;
        Some(index)
    };
    let exact = |a: &str, b: &str| normalized(a) == normalized(b);
    let contains = |a: &str, b: &str| {
        let (a, b) = (normalized(a), normalized(b));
        !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
    };
    // Exact names claim their entries before any looser match does.
    let exact_matches: Vec<Option<usize>> =
        checklist.iter().map(|item| find(item, &exact)).collect();
    let mut delivery: Vec<CookDeliveryItem> = checklist
        .iter()
        .zip(exact_matches)
        .map(
            |(item, matched)| match matched.or_else(|| find(item, &contains)) {
                Some(index) => CookDeliveryItem {
                    item: item.clone(),
                    ..reported[index].clone()
                },
                None => CookDeliveryItem {
                    item: item.clone(),
                    status: CookDeliveryItemStatus::NotDelivered,
                    reason: Some(UNREPORTED_DELIVERY_REASON.to_string()),
                },
            },
        )
        .collect();
    delivery.extend(
        reported
            .iter()
            .zip(used)
            .filter(|(_, used)| !used)
            .map(|(entry, _)| entry.clone()),
    );
    delivery
}

/// Append the task's delivery items to the provider instructions, so the
/// agent accounts for each under its exact text.
pub fn describe(request: &mut AgentTaskRequest) {
    let items = request_delivery_items(request);
    if items.is_empty() {
        return;
    }
    request.instructions.push_str("\n\nHomeboy delivery items: the task checklist Homeboy read from the task source. Report each in `review_form.delivery` under its exact text; an item left unreported is recorded as not_delivered:\n");
    request
        .instructions
        .push_str(&serde_json::to_string(&items).expect("serializable delivery items"));
}

/// The text of a Markdown ATX heading or of a line that is only bold text.
fn heading_text(line: &str) -> Option<String> {
    let text = if line.starts_with('#') {
        let rest = line.trim_start_matches('#');
        if !rest.is_empty() && !rest.starts_with(' ') {
            return None;
        }
        rest.trim().trim_end_matches('#')
    } else if line.len() > 4
        && ((line.starts_with("**") && line.trim_end_matches(':').ends_with("**"))
            || (line.starts_with("__") && line.trim_end_matches(':').ends_with("__")))
    {
        line
    } else {
        return None;
    };
    Some(
        text.trim_matches(|c: char| c == '*' || c == '_' || c == ':' || c.is_whitespace())
            .to_string(),
    )
}

/// The text of a bullet (`-`, `*`, `+`) or ordered (`1.`, `1)`) list item.
fn list_item(line: &str) -> Option<&str> {
    if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        return Some(rest.trim());
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    line[digits..]
        .strip_prefix(". ")
        .or_else(|| line[digits..].strip_prefix(") "))
        .map(str::trim)
}

/// The text of a task-list item (`[ ]`, `[x]`).
fn task_item(text: &str) -> Option<&str> {
    ["[ ]", "[x]", "[X]"]
        .iter()
        .find_map(|marker| text.strip_prefix(marker))
        .map(str::trim)
}

/// The bold lead-in of an item such as `**Bind evidence at intake.** Detail…`,
/// which names the item; otherwise the whole text.
fn bold_title(text: &str) -> &str {
    text.strip_prefix("**")
        .and_then(|rest| rest.split_once("**"))
        .map(|(title, _)| title.trim().trim_end_matches(['.', ':']))
        .filter(|title| !title.is_empty())
        .unwrap_or(text)
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Case-, punctuation- and whitespace-insensitive form used to match items.
fn normalized(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_items_extract_from_checklist_headings_and_task_lists() {
        let cases: [(&str, &str, &[&str]); 7] = [
            ("no checklist", "Fix the crash.\n\n- an aside", &[]),
            (
                "required behaviour heading",
                "Intro\n\n## Required behaviour\n- Parse the file\n- Report errors\n  - nested detail\n\n## Notes\n- not an item",
                &["Parse the file", "Report errors"],
            ),
            (
                "acceptance criteria bold heading with ordered items",
                "**Acceptance criteria:**\n1. Draft on partial\n2) Settles succeeded_partial\n",
                &["Draft on partial", "Settles succeeded_partial"],
            ),
            (
                "task-list items anywhere",
                "Context\n- [ ] Add the test\n  - [x] Keep   the docs\n* [X] Ship it",
                &["Add the test", "Keep the docs", "Ship it"],
            ),
            (
                "heading and task list combined, deduplicated",
                "### Required behavior\n- [ ] Reconcile delivery\n- List in prompt\n\n# Other\n- [ ] reconcile delivery",
                &["Reconcile delivery", "List in prompt"],
            ),
            (
                "rework heading with numbered bold-titled asks",
                "## Rework 2 (required): slice 1 is inert\nContext paragraph.\n\n1. **Bind evidence at intake, not after rewrites.** Detail.\n2. **Accept `box-tree/v3`.** Detail.\n\nAcceptance is unchanged.\n\n## Summary\n- Not an ask",
                &["Bind evidence at intake, not after rewrites", "Accept `box-tree/v3`"],
            ),
            (
                "fenced code is skipped",
                "## Acceptance criteria\n```\n- not an item\n```\n- Real item",
                &["Real item"],
            ),
        ];
        for (name, body, expected) in cases {
            assert_eq!(checklist_items(body), expected, "{name}");
        }
    }

    #[test]
    fn delivery_reconciles_against_the_task_checklist() {
        let delivered = |item: &str| CookDeliveryItem {
            item: item.to_string(),
            status: CookDeliveryItemStatus::Delivered,
            reason: None,
        };
        let not_delivered = |item: &str, reason: &str| CookDeliveryItem {
            item: item.to_string(),
            status: CookDeliveryItemStatus::NotDelivered,
            reason: Some(reason.to_string()),
        };
        let checklist = |items: &[&str]| items.iter().map(|i| i.to_string()).collect::<Vec<_>>();
        let cases: Vec<(
            &str,
            Vec<String>,
            Vec<CookDeliveryItem>,
            Vec<CookDeliveryItem>,
        )> = vec![
            (
                "no checklist keeps the report",
                Vec::new(),
                vec![delivered("Anything")],
                vec![delivered("Anything")],
            ),
            (
                "no checklist and no report stays empty",
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            (
                "every item reported, matched case- and punctuation-insensitively",
                checklist(&["Parse the file", "Report errors"]),
                vec![
                    delivered("report errors."),
                    not_delivered("Parse the `file`", "no fixture"),
                ],
                vec![
                    not_delivered("Parse the file", "no fixture"),
                    delivered("Report errors"),
                ],
            ),
            (
                "unreported item becomes not_delivered",
                checklist(&["Parse the file", "Report errors"]),
                vec![delivered("Parse the file")],
                vec![
                    delivered("Parse the file"),
                    not_delivered("Report errors", UNREPORTED_DELIVERY_REASON),
                ],
            ),
            (
                "empty report leaves every item not_delivered",
                checklist(&["Parse the file"]),
                Vec::new(),
                vec![not_delivered("Parse the file", UNREPORTED_DELIVERY_REASON)],
            ),
            (
                "paraphrase matches by containment, extras follow",
                checklist(&["Report errors"]),
                vec![delivered("Report errors to stderr"), delivered("Extra")],
                vec![delivered("Report errors"), delivered("Extra")],
            ),
        ];
        for (name, checklist, reported, expected) in cases {
            assert_eq!(
                reconcile_delivery(&checklist, &reported),
                expected,
                "{name}"
            );
        }
    }
}
