use homeboy::core::refactor;

#[path = "../support/mod.rs"]
#[allow(dead_code)]
mod support;

#[test]
fn test_build_plan() {
    let root = support::temp_dir("refactor-missing");

    let result = refactor::build_plan("src/missing.rs", root.path(), "grouped", true);
    assert!(result.is_err());
}

#[test]
fn test_apply_plan_skeletons() {
    let root = support::temp_dir("refactor-skeletons");

    let plan = refactor::DecomposePlan {
        file: "src/core/deploy.rs".to_string(),
        strategy: "grouped".to_string(),
        audit_safe: true,
        total_items: 2,
        groups: vec![
            refactor::DecomposeGroup {
                name: "types".to_string(),
                suggested_target: "src/core/deploy/types.inc".to_string(),
                item_names: vec!["DeployConfig".to_string()],
            },
            refactor::DecomposeGroup {
                name: "execution".to_string(),
                suggested_target: "src/core/deploy/execution.inc".to_string(),
                item_names: vec!["run".to_string()],
            },
        ],
        projected_audit_impact: refactor::DecomposeAuditImpact {
            estimated_new_files: 2,
            estimated_new_test_files: 0,
            recommended_test_files: vec![],
            likely_findings: vec![],
        },
        checklist: vec![],
        warnings: vec![],
    };

    let created = refactor::apply_plan_skeletons(&plan, root.path()).unwrap();
    assert_eq!(created.len(), 2);
    assert!(root.path().join("src/core/deploy/types.inc").exists());
    assert!(root.path().join("src/core/deploy/execution.inc").exists());
}
