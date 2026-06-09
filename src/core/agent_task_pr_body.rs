use crate::core::agent_task_finalization::AgentTaskPrFinalizationOptions;
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};
use crate::core::proof::{
    gate_status_label, is_ci_equivalent_gate, HomeboyProof, HomeboyProofGapKind, HomeboyProofRunner,
};

pub(crate) fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    proof: &HomeboyProof,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Proof provenance\n{}\n\n## Source refs\n{}\n\n## Attempt summary\n{}\n\n## Gate results\n{}\n\n## CI-equivalent coverage\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        proof_provenance(proof),
        bullets(&options.evidence.source_refs),
        options.evidence.attempt_summary,
        gate_bullets(&proof.gates),
        ci_coverage_bullets(proof),
        bullets(changed_files),
        bullets(&options.evidence.artifact_refs),
        options.base,
        head,
        options.evidence.ai_tool,
        options.ai_used_for
    )
}

fn proof_provenance(proof: &HomeboyProof) -> String {
    let mut lines = vec![
        format!(
            "- **Proof runner:** {}",
            proof_runner_label(proof.provenance.runner)
        ),
        format!("- **Proof ID:** `{}`", proof.id),
    ];
    if let Some(run_id) = proof.provenance.run_id.as_deref() {
        lines.push(format!("- **Homeboy run ID:** `{run_id}`"));
    }
    if proof.provenance.runner == HomeboyProofRunner::Homeboy {
        lines.push("- **Manual proof:** none recorded by this Homeboy finalization".to_string());
    }
    if !proof.artifacts.is_empty() {
        lines.push("- **Stable evidence:** see artifact refs below".to_string());
    }
    lines.join("\n")
}

fn proof_runner_label(runner: HomeboyProofRunner) -> &'static str {
    match runner {
        HomeboyProofRunner::Homeboy => "Homeboy agent-task cook loop",
        HomeboyProofRunner::Manual => "manual",
        HomeboyProofRunner::ExternalCi => "external CI",
        HomeboyProofRunner::Unknown => "unknown",
    }
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

fn ci_coverage_bullets(proof: &HomeboyProof) -> String {
    let ci_gates: Vec<&HomeboyGateResult> = proof
        .gates
        .iter()
        .filter(|gate| is_ci_equivalent_gate(gate))
        .collect();
    if ci_gates.is_empty() {
        return proof
            .gaps
            .iter()
            .find(|gap| gap.kind == HomeboyProofGapKind::CiEquivalentNotRecorded)
            .map(|gap| format!("- **CI-equivalent required gate:** {}", gap.summary))
            .unwrap_or_else(|| {
                "- **CI-equivalent required gate:** not recorded by Homeboy".to_string()
            });
    }

    ci_gates
        .iter()
        .map(|gate| format!("- {}: {}", gate.name, gate_status_label(gate.status)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn proof_scope(gate: &HomeboyGateResult) -> &'static str {
    if is_ci_equivalent_gate(gate) {
        "CI-equivalent"
    } else {
        "targeted"
    }
}
