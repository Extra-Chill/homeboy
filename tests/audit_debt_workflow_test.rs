fn audit_debt_workflow() -> &'static str {
    include_str!("../.github/workflows/audit-debt.yml")
}

fn full_audit_gate() -> &'static str {
    let workflow = audit_debt_workflow();
    let start = workflow
        .find("  full-audit-gate:\n")
        .expect("full audit gate");
    let end = workflow[start..]
        .find("  full-audit:\n")
        .expect("debt triage job")
        + start;
    &workflow[start..end]
}

#[test]
fn post_merge_full_audit_is_a_blocking_gate() {
    let workflow = audit_debt_workflow();
    let gate = full_audit_gate();

    assert!(workflow.contains("push:\n    branches: [main]"));
    assert!(gate.contains("if: github.event_name == 'push'"));
    assert!(gate.contains("cargo run --locked -- review audit homeboy --profile=full"));
    assert!(!gate.contains("continue-on-error: true"));
}

#[test]
fn scheduled_debt_triage_remains_non_blocking_and_separate() {
    let workflow = audit_debt_workflow();

    assert!(workflow.contains("full-audit:\n    name: Full-tree audit → tracking issues"));
    assert!(workflow.contains("if: github.event_name != 'push'"));
    assert!(workflow.contains("auto-issue: 'true'"));
}
