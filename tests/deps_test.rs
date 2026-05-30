include!("core/deps_test.rs");

#[test]
fn stack_apply_dry_run_passes_constraint_to_default_provider_update() {
    let plan = deps::DependencyStackPlan::new(
        "upstream",
        vec![deps::DependencyStackPlanStep {
            sequence: 1,
            declaring_component_id: "downstream".to_string(),
            upstream: "upstream".to_string(),
            downstream: "downstream".to_string(),
            downstream_path: "/tmp/downstream path".to_string(),
            package: "fixture/upstream".to_string(),
            update_command: "homeboy deps update fixture/upstream --path '/tmp/downstream path'"
                .to_string(),
            rebuild: false,
            post_update: Vec::new(),
            test: Vec::new(),
        }],
    );

    let result = deps::stack_apply_plan(plan, Some("^2.0"), true, false, false).unwrap();

    assert_eq!(result.step_count, 1);
    assert_eq!(
        result.steps[0].command_results[0].command,
        "homeboy deps update fixture/upstream --path '/tmp/downstream path' --to '^2.0' --no-install"
    );
}
