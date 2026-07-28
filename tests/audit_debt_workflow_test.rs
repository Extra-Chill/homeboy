fn audit_debt_workflow() -> &'static str {
    include_str!("../.github/workflows/audit-debt.yml")
}

#[test]
fn audit_debt_is_scheduled_or_manual_not_a_post_merge_guard() {
    let workflow = audit_debt_workflow();

    assert!(workflow.contains("schedule:"));
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(!workflow.contains("push:"));
    assert!(!workflow.contains("Main Guard"));
    assert!(!workflow.contains("qualification_sha"));
}

#[test]
fn audit_debt_remains_an_advisory_full_tree_sweep() {
    let workflow = audit_debt_workflow();

    assert!(workflow.contains("commands: review audit"));
    assert!(workflow.contains("args: --profile=${{ github.event.inputs.profile || 'full' }}"));
    assert!(workflow.contains("auto-issue: 'true'"));
    assert!(!workflow.contains("review lint"));
    assert!(!workflow.contains("review test"));
}

#[test]
fn audit_debt_never_mutates_the_repository() {
    let workflow = audit_debt_workflow();

    assert!(!workflow.contains("git push"));
    assert!(!workflow.contains("pr-policy-merge: 'true'"));
    assert!(!workflow.contains("contents: write"));
}
