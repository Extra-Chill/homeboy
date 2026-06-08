fn release_workflow() -> &'static str {
    include_str!("../.github/workflows/release.yml")
}

fn job_section<'a>(workflow: &'a str, job: &str) -> &'a str {
    let marker = format!("  {job}:\n");
    let start = workflow
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {job} job"));
    let rest = &workflow[start + marker.len()..];
    let end = rest
        .lines()
        .scan(0usize, |offset, line| {
            let current = *offset;
            *offset += line.len() + 1;
            Some((current, line))
        })
        .skip(1)
        .find_map(|(offset, line)| {
            let is_next_job = line.starts_with("  ")
                && !line.starts_with("    ")
                && line.trim_end().ends_with(':');

            is_next_job.then_some(offset.saturating_sub(1))
        })
        .unwrap_or(rest.len());

    &rest[..end]
}

#[test]
fn release_refactor_gate_enables_non_pr_repair_path() {
    let gate_refactor = job_section(release_workflow(), "gate-refactor");

    assert!(gate_refactor.contains("commands: ${{ env.RELEASE_BLOCKING_COMMANDS }}"));
    assert!(gate_refactor.contains("autofix: 'true'"));
    assert!(gate_refactor.contains("autofix-mode: always"));
    assert!(gate_refactor.contains("Only release-blocking commands are repaired"));
    assert!(gate_refactor.contains("autofix-open-pr: 'true'"));
    assert!(gate_refactor.contains("continue-on-error: true"));
}

#[test]
fn release_quality_policy_defaults_to_lint_and_test_blocking() {
    assert!(release_workflow().contains(
        "RELEASE_BLOCKING_COMMANDS: ${{ inputs.release_blocking_commands || 'lint,test' }}"
    ));

    let policy = job_section(release_workflow(), "release-quality-policy");

    assert!(policy.contains("BLOCKING_COMMANDS: ${{ env.RELEASE_BLOCKING_COMMANDS }}"));
    assert!(policy.contains(
        "AUDIT_RESULT: ${{ needs.gate-audit.outputs.audit-result || needs.gate-audit.result }}"
    ));
    assert!(policy.contains("check_command audit"));
    assert!(policy.contains("check_command lint"));
    assert!(policy.contains("check_command test"));
    assert!(policy.contains("Command ${command} is tracked but not release-blocking"));
}

#[test]
fn release_audit_is_advisory_without_losing_raw_failure_outcome() {
    let gate_audit = job_section(release_workflow(), "gate-audit");

    assert!(gate_audit.contains("audit-result: ${{ steps.audit.outcome }}"));
    assert!(gate_audit.contains("id: audit"));
    assert!(gate_audit.contains("continue-on-error: true"));
}

#[test]
fn release_prepare_waits_for_command_policy_not_raw_audit_or_refactor() {
    let gate_refactor = job_section(release_workflow(), "gate-refactor");
    let prepare = job_section(release_workflow(), "prepare");

    assert!(
        !gate_refactor.contains("- gate-audit"),
        "gate-refactor should not wait on tracked-only audit by default"
    );

    for gate in ["gate-lint", "gate-test"] {
        assert!(
            gate_refactor.contains(&format!("- {gate}")),
            "gate-refactor should inspect the read-only {gate} result for advisory repair"
        );
    }

    for gate in ["gate-audit", "gate-lint", "gate-test"] {
        assert!(
            !prepare.contains(&format!("- {gate}")),
            "prepare should wait on release-quality-policy, not raw {gate}"
        );
    }

    assert!(prepare.contains("- release-quality-policy"));
    assert!(!prepare.contains("- gate-refactor"));
    assert!(prepare.contains("needs.release-quality-policy.result == 'success'"));
    assert!(prepare.contains("inputs.release_tag != ''"));
}

#[test]
fn release_prepare_uses_prepared_output_to_unlock_publish_jobs() {
    let prepare = job_section(release_workflow(), "prepare");
    let plan = job_section(release_workflow(), "plan");

    assert!(prepare.contains(
        "prepared: ${{ steps.recovery.outputs.prepared || steps.prepared.outputs.prepared }}"
    ));
    assert!(prepare.contains("id: prepared"));
    assert!(prepare.contains("steps.release.outputs.release-tag != ''"));
    assert!(prepare.contains("echo \"prepared=true\" >> \"$GITHUB_OUTPUT\""));
    assert!(prepare.contains("downstream jobs will publish it in this run"));
    assert!(
        prepare.contains("Skipping release preparation; downstream jobs will publish existing tag")
    );

    assert!(plan.contains("needs.prepare.outputs.prepared == 'true'"));
    assert!(plan.contains("needs.prepare.outputs.release-tag != ''"));
    assert!(!plan.contains("needs.prepare.outputs.released == 'true'"));
}

#[test]
fn release_test_gate_exposes_release_blocking_policy_to_rust_tests() {
    let gate_test = job_section(release_workflow(), "gate-test");

    assert!(gate_test.contains("RELEASE_BLOCKING_COMMANDS: ${{ env.RELEASE_BLOCKING_COMMANDS }}"));
}
