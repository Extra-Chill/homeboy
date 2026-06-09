use crate::core::agent_task_finalization::AgentTaskPrFinalizationOptions;
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

pub(crate) fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Source refs\n{}\n\n{}{}## Attempt summary\n{}\n\n## Gate results\n{}\n\n## Verification capability\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n{}## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Model:** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        bullets(&options.evidence.source_refs),
        source_relationship_section(options),
        runtime_guardrails_section(options),
        options.evidence.attempt_summary,
        gate_bullets(&normalized_gate_results(options)),
        verification_bullets(options),
        bullets(changed_files),
        bullets(&options.evidence.artifact_refs),
        supersession_note(options),
        options.base,
        head,
        options.evidence.ai_tool,
        options
            .evidence
            .ai_model
            .as_deref()
            .unwrap_or("not recorded by provider metadata"),
        options.ai_used_for
    )
}

fn bullets(values: &[String]) -> String {
    if values.is_empty() {
        return "- none recorded".to_string();
    }
    values
        .iter()
        .map(|value| format!("- {}", value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn gate_bullets(gates: &[HomeboyGateResult]) -> String {
    gates
        .iter()
        .map(|gate| match gate.status {
            HomeboyGateStatus::Passed => format!("- {}: passed", gate.name),
            HomeboyGateStatus::Failed => format!("- {}: failed", gate.name),
            HomeboyGateStatus::Skipped => format!("- {}: skipped", gate.name),
            HomeboyGateStatus::Blocked => format!("- {}: blocked", gate.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn source_relationship_section(options: &AgentTaskPrFinalizationOptions) -> String {
    let relationship = &options.evidence.source_relationship;
    if relationship.is_empty() {
        return String::new();
    }

    format!(
        "## Source relationship\n- **Related finding ID:** {}\n- **Source packet ID:** {}\n- **Change kind:** {}\n- **Supersedes:** {}\n- **Depends on:** {}\n\n",
        option_value(relationship.related_finding_id.as_deref()),
        option_value(relationship.source_packet_id.as_deref()),
        option_value(relationship.change_kind.as_deref()),
        inline_list(&relationship.supersedes),
        inline_list(&relationship.depends_on),
    )
}

fn runtime_guardrails_section(options: &AgentTaskPrFinalizationOptions) -> String {
    let guardrails = &options.evidence.runtime_guardrails;
    if guardrails.is_empty() {
        return String::new();
    }

    format!(
        "## Runtime guardrails\n- **Why this is not broader than the packet evidence:** {}\n- **Evidence discriminators preserved:** {}\n- **Nearby contracts preserved:** {}\n\n",
        option_value(guardrails.why_not_broader_than_packet.as_deref()),
        inline_list(&guardrails.evidence_discriminators),
        inline_list(&guardrails.nearby_contracts_preserved),
    )
}

fn verification_bullets(options: &AgentTaskPrFinalizationOptions) -> String {
    let verification = &options.evidence.verification;
    if verification.is_empty() {
        return "- `targeted_checks_run`: see gate results above".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "- `targeted_checks_run`: {}",
        inline_code_list(&verification.targeted_checks_run)
    ));
    lines.push(format!(
        "- `targeted_checks_unavailable`: {}",
        option_value(verification.targeted_checks_unavailable.as_deref())
    ));
    lines.push(format!(
        "- `ci_expected`: {}",
        inline_code_list(&verification.ci_expected)
    ));
    lines.push(format!(
        "- `manual_reviewer_check`: {}",
        option_value(verification.manual_reviewer_check.as_deref())
    ));
    lines.join("\n")
}

fn supersession_note(options: &AgentTaskPrFinalizationOptions) -> String {
    let relationship = &options.evidence.source_relationship;
    if relationship.supersedes.is_empty() && relationship.depends_on.is_empty() {
        return String::new();
    }

    "## Sibling PR relationship\n- Review sibling generated PRs for the same finding before merging; superseded runtime fixes should be narrowed or closed after safer evidence/test-only coverage lands.\n\n".to_string()
}

fn option_value(value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("none recorded")
        .to_string()
}

fn inline_list(values: &[String]) -> String {
    if values.is_empty() {
        return "none recorded".to_string();
    }
    values.join(", ")
}

fn inline_code_list(values: &[String]) -> String {
    if values.is_empty() {
        return "none recorded".to_string();
    }
    values
        .iter()
        .map(|value| format!("`{}`", value.replace('`', "'")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn normalized_gate_results(options: &AgentTaskPrFinalizationOptions) -> Vec<HomeboyGateResult> {
    if !options.normalized_gate_results.is_empty() {
        return options.normalized_gate_results.clone();
    }

    options
        .gate_results
        .iter()
        .cloned()
        .map(crate::core::agent_task_finalization::gate_result_from_legacy)
        .collect()
}
