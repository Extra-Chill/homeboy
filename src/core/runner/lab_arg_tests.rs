use super::*;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn rewrites_lab_offload_path_and_strips_runner_and_output_flags() {
    let input = args(&[
        "homeboy",
        "audit",
        "--path",
        "/Users/chubes/Developer/project",
        "--runner",
        "lab",
        "--json-summary",
        "--output",
        "/tmp/local.json",
        "--runner=other",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/chubes/Developer/project", &[]),
        args(&[
            "homeboy",
            "--force-hot",
            "audit",
            "--path",
            "/home/chubes/Developer/project",
            "--json-summary",
        ])
    );
}

#[test]
fn rewrites_agent_task_cook_codebox_cwd_to_materialized_checkout() {
    let input = args(&[
        "homeboy",
        "agent-task",
        "cook",
        "--cwd",
        "/Users/chubes/Developer/homeboy@fix-issue-4010-cook-cwd-materialization",
        "--backend",
        "codebox",
        "--runner",
        "lab",
        "--prompt",
        "fix issue 4010",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(
            &input,
            "/home/chubes/_lab_workspaces/homeboy@fix-issue-4010-cook-cwd-materialization-abc",
            &[],
        ),
        args(&[
            "homeboy",
            "--force-hot",
            "agent-task",
            "cook",
            "--cwd",
            "/home/chubes/_lab_workspaces/homeboy@fix-issue-4010-cook-cwd-materialization-abc",
            "--backend",
            "codebox",
            "--prompt",
            "fix issue 4010",
        ])
    );
}

#[test]
fn rewrites_agent_task_cook_codebox_cwd_equals_with_workspace_mapping() {
    let input = args(&[
        "homeboy",
        "agent-task",
        "cook",
        "--cwd=/Users/chubes/Developer/homeboy@fix-issue-4010-cook-cwd-materialization/packages/demo",
        "--backend=codebox",
        "--prompt",
        "fix issue 4010",
    ]);
    let mappings = vec![LabPathRemap {
        local: "/Users/chubes/Developer/homeboy@fix-issue-4010-cook-cwd-materialization"
            .to_string(),
        remote: "/home/chubes/_lab_workspaces/homeboy@fix-issue-4010-cook-cwd-materialization-abc"
            .to_string(),
    }];

    assert_eq!(
        rewrite_lab_offload_args(
            &input,
            "/home/chubes/_lab_workspaces/homeboy@fix-issue-4010-cook-cwd-materialization-abc",
            &mappings,
        ),
        args(&[
            "homeboy",
            "--force-hot",
            "agent-task",
            "cook",
            "--cwd=/home/chubes/_lab_workspaces/homeboy@fix-issue-4010-cook-cwd-materialization-abc/packages/demo",
            "--backend=codebox",
            "--prompt",
            "fix issue 4010",
        ])
    );
}

#[test]
fn leaves_passthrough_path_args_untouched() {
    let input = args(&[
        "homeboy",
        "test",
        "--path=/Users/chubes/Developer/project",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        "--path",
        "test-fixture",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/chubes/Developer/project", &[]),
        args(&[
            "homeboy",
            "--force-hot",
            "test",
            "--path=/home/chubes/Developer/project",
            "--",
            "--path",
            "test-fixture",
        ])
    );
}

#[test]
fn strips_internal_passthrough_sentinel_from_lab_offload_command() {
    let filter = "--filter=ConversationStoreFactoryTest::test_canonical_conversation_session_abilities_route_through_swapped_store";
    let input = args(&[
        "homeboy",
        "test",
        "data-machine",
        "--path",
        "/Users/chubes/Developer/data-machine@fix",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        filter,
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/chubes/Developer/data-machine@fix", &[]),
        args(&[
            "homeboy",
            "--force-hot",
            "test",
            "data-machine",
            "--path",
            "/home/chubes/Developer/data-machine@fix",
            "--",
            filter,
        ])
    );
}

#[test]
fn rewrite_lab_offload_args_does_not_duplicate_force_hot() {
    let input = args(&[
        "homeboy",
        "--force-hot",
        "refactor",
        "--from",
        "audit",
        "--path",
        "/Users/chubes/Developer/project",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/chubes/Developer/project", &[]),
        args(&[
            "homeboy",
            "--force-hot",
            "refactor",
            "--from",
            "audit",
            "--path",
            "/home/chubes/Developer/project",
        ])
    );
}

#[test]
fn detects_lab_offload_source_path_from_path_flag() {
    let input = args(&[
        "homeboy",
        "test",
        "--path",
        "/Users/chubes/Developer/project",
    ]);

    assert_eq!(
        lab_offload_source_path(&input).expect("path"),
        std::path::PathBuf::from("/Users/chubes/Developer/project")
    );
}
