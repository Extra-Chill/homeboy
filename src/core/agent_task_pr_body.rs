use crate::core::agent_task_finalization::AgentTaskPrFinalizationOptions;
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

pub(crate) fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Source refs\n{}\n\n## Attempt summary\n{}\n\n## Gate results\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        bullets(&options.evidence.source_refs),
        options.evidence.attempt_summary,
        gate_bullets(&normalized_gate_results(options)),
        bullets(changed_files),
        bullets(&options.evidence.artifact_refs),
        options.base,
        head,
        options.evidence.ai_tool,
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
