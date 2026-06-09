use crate::core::agent_task_finalization::AgentTaskPrFinalizationOptions;
use crate::core::gate::HomeboyGateResult;
use crate::core::proof::{
    gate_scope_label, gate_status_label, is_ci_equivalent_gate, proof_runner_label, HomeboyProof,
    HomeboyProofGapKind, HomeboyProofRunner,
};

const NONE_RECORDED: &str = "none recorded";
const NONE_RECORDED_BULLET: &str = "- none recorded";
const AI_MODEL_NOT_RECORDED: &str = "not recorded by provider metadata";
const TARGETED_CHECKS_RUN_LABEL: &str = "targeted_checks_run";
const TARGETED_CHECKS_UNAVAILABLE_LABEL: &str = "targeted_checks_unavailable";
const CI_EXPECTED_LABEL: &str = "ci_expected";
const MANUAL_REVIEWER_CHECK_LABEL: &str = "manual_reviewer_check";

pub(crate) fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    proof: &HomeboyProof,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Proof provenance\n{}\n\n## Source refs\n{}\n\n{}{}## Attempt summary\n{}\n\n## Gate results\n{}\n\n## Verification capability\n{}\n\n## CI-equivalent coverage\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n{}## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Model:** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        proof_provenance(proof),
        bullets(&options.evidence.source_refs),
        source_relationship_section(options),
        runtime_guardrails_section(options),
        options.evidence.attempt_summary,
        gate_bullets(&proof.gates),
        verification_bullets(options),
        ci_coverage_bullets(proof),
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
            .unwrap_or(AI_MODEL_NOT_RECORDED),
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

fn bullets(values: &[String]) -> String {
    if values.is_empty() {
        return NONE_RECORDED_BULLET.to_string();
    }
    values
        .iter()
        .map(|value| format!("- {}", value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn gate_bullets(gates: &[HomeboyGateResult]) -> String {
    if gates.is_empty() {
        return NONE_RECORDED_BULLET.to_string();
    }

    gates
        .iter()
        .map(|gate| {
            format!(
                "- {}: {} ({})",
                gate.name,
                gate_status_label(gate.status),
                gate_scope_label(gate)
            )
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
        return format!("- `{TARGETED_CHECKS_RUN_LABEL}`: see gate results above");
    }

    let lines = vec![
        format!(
            "- `{TARGETED_CHECKS_RUN_LABEL}`: {}",
            inline_code_list(&verification.targeted_checks_run)
        ),
        format!(
            "- `{TARGETED_CHECKS_UNAVAILABLE_LABEL}`: {}",
            option_value(verification.targeted_checks_unavailable.as_deref())
        ),
        format!(
            "- `{CI_EXPECTED_LABEL}`: {}",
            inline_code_list(&verification.ci_expected)
        ),
        format!(
            "- `{MANUAL_REVIEWER_CHECK_LABEL}`: {}",
            option_value(verification.manual_reviewer_check.as_deref())
        ),
    ];
    lines.join("\n")
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
        .unwrap_or(NONE_RECORDED)
        .to_string()
}

fn inline_list(values: &[String]) -> String {
    if values.is_empty() {
        return NONE_RECORDED.to_string();
    }
    values.join(", ")
}

fn inline_code_list(values: &[String]) -> String {
    if values.is_empty() {
        return NONE_RECORDED.to_string();
    }
    values
        .iter()
        .map(|value| format!("`{}`", value.replace('`', "'")))
        .collect::<Vec<_>>()
        .join(", ")
}
