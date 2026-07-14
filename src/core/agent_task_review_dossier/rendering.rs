use crate::core::gate::HomeboyGateResult;

use super::model::{
    AgentTaskReviewDossier, AgentTaskReviewEvidence, AgentTaskReviewIssueRelationshipKind,
    AgentTaskReviewProfile, AgentTaskReviewSectionId,
};

pub fn enrich_dossier(
    dossier: &mut AgentTaskReviewDossier,
    source_refs: &[String],
    artifact_refs: &[String],
    gates: &[HomeboyGateResult],
    ci_expected: &[String],
    lifecycle: Option<&crate::core::run_lifecycle_record::RunLifecycleRecord>,
) {
    for gate in gates {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: format!("{}: {:?}", gate.name, gate.status),
            url: None,
        });
    }
    for check in ci_expected {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: format!("CI expected: {check}"),
            url: None,
        });
    }
    if let Some(lifecycle) = lifecycle {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: format!("Durable run execution: {:?}", lifecycle.execution.state),
            url: None,
        });
    }
    for reference in source_refs.iter().chain(artifact_refs) {
        if is_reviewer_url(reference) {
            dossier.evidence.push(AgentTaskReviewEvidence {
                summary: "Reviewer-resolvable source evidence".to_string(),
                url: Some(reference.clone()),
            });
        }
    }
    dossier
        .evidence
        .sort_by(|a, b| a.summary.cmp(&b.summary).then(a.url.cmp(&b.url)));
    dossier.evidence.dedup();
}

pub fn render_review_dossier(
    dossier: &AgentTaskReviewDossier,
    profile: &AgentTaskReviewProfile,
) -> String {
    let mut sections = Vec::new();
    for id in ordered_sections(profile) {
        if profile.hidden_sections.contains(&id) {
            continue;
        }
        let section = match id {
            AgentTaskReviewSectionId::Summary if !dossier.summary.is_empty() => {
                Some(("Summary", prose(&dossier.summary)))
            }
            AgentTaskReviewSectionId::WhatChanged if !dossier.what_changed.is_empty() => {
                Some(("What changed", bullets(&dossier.what_changed)))
            }
            AgentTaskReviewSectionId::HowToTest if !dossier.how_to_test.is_empty() => Some((
                "How to test",
                dossier
                    .how_to_test
                    .iter()
                    .enumerate()
                    .map(|(i, step)| {
                        format!(
                            "{}. Run `{}`; expect {}.",
                            i + 1,
                            code(&step.command),
                            prose(&step.expected)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            AgentTaskReviewSectionId::Compatibility if !dossier.compatibility.is_empty() => {
                Some(("Compatibility", prose(&dossier.compatibility)))
            }
            AgentTaskReviewSectionId::Evidence if !dossier.evidence.is_empty() => Some((
                "Evidence",
                dossier
                    .evidence
                    .iter()
                    .map(|item| match &item.url {
                        Some(url) => format!("- {}: {url}", prose(&item.summary)),
                        None => format!("- {}", prose(&item.summary)),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            AgentTaskReviewSectionId::AiAssistance => Some((
                "AI assistance",
                format!(
                    "- **AI assistance:** {}\n- **Tool(s):** {}\n- **Model:** {}\n- **Used for:** {}",
                    if dossier.ai_assistance.used {
                        "Yes"
                    } else {
                        "No"
                    },
                    prose(&dossier.ai_assistance.tool),
                    prose(&dossier.ai_assistance.model),
                    prose(&dossier.ai_assistance.used_for)
                ),
            )),
            AgentTaskReviewSectionId::SourceRelationships
                if !dossier.source_relationships.is_empty() =>
            {
                Some((
                    "Source relationships",
                    dossier
                        .source_relationships
                        .iter()
                        .map(|item| {
                            format!(
                                "- {} {}",
                                match item.kind {
                                    AgentTaskReviewIssueRelationshipKind::Closes => "Closes",
                                    AgentTaskReviewIssueRelationshipKind::RelatesTo => "Relates to",
                                },
                                relationship_reference(&item.reference)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ))
            }
            _ => None,
        };
        if let Some((heading, content)) = section {
            sections.push(format!("## {heading}\n{content}"));
        }
    }
    for section in &profile.additional_sections {
        if !section.content.is_empty() {
            sections.push(format!(
                "## {}\n{}",
                section.heading,
                prose(&section.content)
            ));
        }
    }
    sections.join("\n\n") + "\n"
}

fn ordered_sections(profile: &AgentTaskReviewProfile) -> Vec<AgentTaskReviewSectionId> {
    let mut sections = profile.section_order.clone();
    for id in [
        AgentTaskReviewSectionId::Summary,
        AgentTaskReviewSectionId::WhatChanged,
        AgentTaskReviewSectionId::HowToTest,
        AgentTaskReviewSectionId::Compatibility,
        AgentTaskReviewSectionId::Evidence,
        AgentTaskReviewSectionId::AiAssistance,
        AgentTaskReviewSectionId::SourceRelationships,
    ] {
        if !sections.contains(&id) {
            sections.push(id);
        }
    }
    sections
}

fn prose(value: &str) -> String {
    let mut rendered: String = value
        .chars()
        .flat_map(|character| match character {
            '*' | '_' | '`' | '[' | ']' | '<' | '>' | '!' => vec!['\\', character],
            _ => vec![character],
        })
        .collect();
    if rendered.starts_with(['#', '>', '-', '+'])
        || rendered
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
            && rendered.chars().nth(1) == Some('.')
    {
        rendered.insert(0, '\\');
    }
    rendered
}

fn code(value: &str) -> String {
    value.replace('`', "'")
}

fn relationship_reference(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("https://github.com/") {
        if let Some((repository, number)) = rest.split_once("/issues/") {
            return format!("{repository}#{number}");
        }
    }
    value.to_string()
}

fn bullets(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("- {}", prose(value)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_reviewer_url(value: &str) -> bool {
    value.starts_with("https://")
}
