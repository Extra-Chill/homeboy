use crate::core::agent_task_finalization::{AgentTaskGateResult, AgentTaskPrFinalizationOptions};

pub(crate) fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Source refs\n{}\n\n## Attempt summary\n{}\n\n## Gate results\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        bullets(&options.source_refs),
        options.attempt_summary,
        gate_bullets(&options.gate_results),
        bullets(changed_files),
        bullets(&options.artifact_refs),
        options.base,
        head,
        options.ai_tool,
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

fn gate_bullets(gates: &[AgentTaskGateResult]) -> String {
    gates
        .iter()
        .map(|gate| match &gate.detail {
            Some(detail) if !detail.trim().is_empty() => {
                format!("- {}: {} ({})", gate.name, gate.status, detail)
            }
            _ => format!("- {}: {}", gate.name, gate.status),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
