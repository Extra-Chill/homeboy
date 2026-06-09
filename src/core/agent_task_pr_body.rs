use crate::core::agent_task_finalization::AgentTaskPrFinalizationOptions;
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

pub(crate) fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Proof provenance\n{}\n\n## Source refs\n{}\n\n## Attempt summary\n{}\n\n## Gate results\n{}\n\n## CI-equivalent coverage\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        proof_provenance(options),
        bullets(&options.evidence.source_refs),
        options.evidence.attempt_summary,
        gate_bullets(&normalized_gate_results(options)),
        ci_coverage_bullets(&normalized_gate_results(options)),
        bullets(changed_files),
        bullets(&options.evidence.artifact_refs),
        options.base,
        head,
        options.evidence.ai_tool,
        options.ai_used_for
    )
}

fn proof_provenance(options: &AgentTaskPrFinalizationOptions) -> String {
    let mut lines = vec![
        "- **Proof runner:** Homeboy agent-task cook loop".to_string(),
        format!("- **Homeboy run ID:** `{}`", options.run_id),
        "- **Manual proof:** none recorded by this Homeboy finalization".to_string(),
    ];
    if !options.evidence.artifact_refs.is_empty() {
        lines.push("- **Stable evidence:** see artifact refs below".to_string());
    }
    lines.join("\n")
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
    if gates.is_empty() {
        return "- none recorded".to_string();
    }

    gates
        .iter()
        .map(|gate| match gate.status {
            HomeboyGateStatus::Passed => format!("- {}: passed ({})", gate.name, proof_scope(gate)),
            HomeboyGateStatus::Failed => format!("- {}: failed ({})", gate.name, proof_scope(gate)),
            HomeboyGateStatus::Skipped => {
                format!("- {}: skipped ({})", gate.name, proof_scope(gate))
            }
            HomeboyGateStatus::Blocked => {
                format!("- {}: blocked ({})", gate.name, proof_scope(gate))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ci_coverage_bullets(gates: &[HomeboyGateResult]) -> String {
    let ci_gates: Vec<&HomeboyGateResult> =
        gates.iter().filter(|gate| is_ci_equivalent(gate)).collect();
    if ci_gates.is_empty() {
        return "- **CI-equivalent required gate:** not recorded by Homeboy; targeted proof must not be treated as full CI coverage".to_string();
    }

    ci_gates
        .iter()
        .map(|gate| format!("- {}: {}", gate.name, status_label(gate.status)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn proof_scope(gate: &HomeboyGateResult) -> &'static str {
    if is_ci_equivalent(gate) {
        "CI-equivalent"
    } else {
        "targeted"
    }
}

fn status_label(status: HomeboyGateStatus) -> &'static str {
    match status {
        HomeboyGateStatus::Passed => "passed",
        HomeboyGateStatus::Failed => "failed",
        HomeboyGateStatus::Skipped => "skipped",
        HomeboyGateStatus::Blocked => "blocked",
    }
}

fn is_ci_equivalent(gate: &HomeboyGateResult) -> bool {
    gate.provenance
        .get("ci_equivalent")
        .and_then(|value| value.as_bool())
        .or_else(|| {
            gate.evidence
                .get("ci_equivalent")
                .and_then(|value| value.as_bool())
        })
        .unwrap_or(false)
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
