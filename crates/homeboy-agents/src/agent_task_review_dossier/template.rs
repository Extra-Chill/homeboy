use super::*;
use std::path::Path;

/// GitHub's single-template locations, in precedence order. Sort entries so
/// case variants have deterministic precedence even on case-sensitive disks.
fn discover(worktree: &Path) -> Option<String> {
    for directory in [".github", "docs", ""] {
        let Ok(entries) = std::fs::read_dir(worktree.join(directory)) else {
            continue;
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path.file_name().is_some_and(|name| {
                        name.to_string_lossy()
                            .eq_ignore_ascii_case("pull_request_template.md")
                    })
            })
            .collect::<Vec<_>>();
        paths.sort();
        if let Some(text) = paths
            .first()
            .and_then(|path| std::fs::read_to_string(path).ok())
        {
            return Some(text);
        }
    }
    None
}

/// Render against the candidate checkout, not the controller repository.
pub fn render_review_dossier_for_worktree(
    dossier: &AgentTaskReviewDossier,
    profile: &AgentTaskReviewProfile,
    worktree: &Path,
) -> String {
    let mut body = match discover(worktree) {
        Some(template) => render_template(dossier, &template),
        None => render_review_dossier(dossier, profile),
    };
    if let Ok(Some(warning)) =
        crate::agent_task_promotion::large_untracked_warning(&worktree.to_string_lossy())
    {
        body.push_str(&format!("\n\n> **Candidate size warning:** {warning}\n"));
    }
    body
}

fn heading(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let text = line.trim_start_matches('#');
    (text.len() != line.len() && text.starts_with(' '))
        .then(|| text.trim().trim_end_matches('#').trim())
}

// Deliberately small vocabulary: repositories own the headings and order;
// Homeboy only knows the kinds of review data it can supply.
fn section_content(dossier: &AgentTaskReviewDossier, title: &str) -> Option<String> {
    let words = title.to_ascii_lowercase();
    let words = words
        .split(|c: char| !c.is_alphanumeric())
        .collect::<Vec<_>>();
    let has = |keywords: &[&str]| words.iter().any(|word| keywords.contains(word));
    if has(&["checklist"]) {
        None
    } else if has(&["ai", "artificial"]) {
        dossier.ai_assistance.used.then(|| {
            render_ai_disclosure(&dossier.ai_assistance)
                .split_once('\n')
                .expect("AI disclosure has a heading")
                .1
                .to_string()
        })
    } else if has(&["issue", "issues", "tracker", "links"]) {
        let relationships = render_source_relationships(&dossier.source_relationships);
        let links = render_evidence(
            &dossier
                .evidence
                .iter()
                .filter(|item| item.url.is_some())
                .cloned()
                .collect::<Vec<_>>(),
        );
        Some(
            [relationships, links]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    } else if has(&["test", "testing", "verification"]) {
        let mut content = render_verification(dossier).unwrap_or_default();
        for step in &dossier.how_to_test {
            if reviewer_runnable_command(&step.command) {
                content.push_str(&format!(
                    "\n- {}; expect {}.",
                    render_command(&step.command),
                    prose(&step.expected)
                ));
            }
        }
        Some(content)
    } else if has(&["summary", "changes", "description"]) {
        Some(format!(
            "{}\n\n{}",
            prose(&dossier.summary),
            bullets(&dossier.what_changed)
        ))
    } else {
        None
    }
}

fn render_template(dossier: &AgentTaskReviewDossier, template: &str) -> String {
    let unchecked =
        Regex::new(r"(?m)^(\s*(?:[-*+]|\d+\.)\s+)\[[xX]\]").expect("valid checklist regex");
    let template = unchecked.replace_all(template, "${1}[ ]");
    let lines = template.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        output.push(line.to_string());
        index += 1;
        let Some(title) = heading(line) else { continue };
        let end = (index..lines.len())
            .find(|&i| heading(lines[i]).is_some())
            .unwrap_or(lines.len());
        if let Some(content) =
            section_content(dossier, title).filter(|text| !text.trim().is_empty())
        {
            output.push(String::new());
            output.push(content);
            // Never claim a repository checklist has been completed, or drop it
            // merely because it lives in an otherwise mapped section.
            output.extend(
                lines[index..end]
                    .iter()
                    .filter(|line| line.contains("[ ]"))
                    .map(|line| line.to_string()),
            );
            output.push(String::new());
        } else {
            output.extend(lines[index..end].iter().map(|line| line.to_string()));
        }
        index = end;
    }
    output.join("\n").trim_end().to_string() + "\n"
}
