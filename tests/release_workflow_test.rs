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
fn release_refactor_gate_opens_repair_prs() {
    let gate_refactor = job_section(release_workflow(), "gate-refactor");

    assert!(gate_refactor.contains("commands: audit,lint,test"));
    assert!(gate_refactor.contains("autofix: 'true'"));
    assert!(gate_refactor.contains("autofix-mode: always"));
    assert!(gate_refactor.contains("autofix-open-pr: 'true'"));
}

#[test]
fn release_prepare_waits_for_repaired_quality_state() {
    let gate_refactor = job_section(release_workflow(), "gate-refactor");
    let prepare = job_section(release_workflow(), "prepare");

    for gate in ["gate-audit", "gate-lint", "gate-test"] {
        assert!(
            gate_refactor.contains(&format!("- {gate}")),
            "gate-refactor should inspect the read-only {gate} result before repair"
        );
        assert!(
            !prepare.contains(&format!("- {gate}")),
            "prepare should wait on gate-refactor, not fail directly on {gate}"
        );
    }

    assert!(prepare.contains("- gate-refactor"));
    assert!(prepare.contains("needs.gate-refactor.result == 'success'"));
    assert!(prepare.contains("inputs.release_tag != ''"));
}
