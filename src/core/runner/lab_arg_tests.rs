use super::super::lab_args::{
    lab_offload_source_path, rewrite_lab_offload_args, rewrite_runner_resident_lab_offload_args,
    EXPLICIT_PASSTHROUGH_SENTINEL,
};

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn rewrites_lab_offload_path_and_strips_runner_and_output_flags() {
    let input = args(&[
        "homeboy",
        "audit",
        "--path",
        "/Users/user/Developer/project",
        "--runner",
        "lab",
        "--json-summary",
        "--output",
        "/tmp/local.json",
        "--runner=other",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "audit",
            "--path",
            "/home/user/Developer/project",
            "--json-summary",
        ])
    );
}

#[test]
fn maps_command_output_path_to_runner_output_path() {
    let input = args(&[
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "controller",
        "run-from-spec",
        "loop.json",
        "--max-actions",
        "1",
        "--output",
        "/tmp/local-result.json",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(
            &input,
            "/home/user/Developer/project",
            &[],
            Some("/home/user/Developer/project/homeboy-lab-structured-output.json"),
        ),
        args(&[
            "homeboy",
            "--force-hot",
            "agent-task",
            "controller",
            "run-from-spec",
            "loop.json",
            "--max-actions",
            "1",
            "--output",
            "/home/user/Developer/project/homeboy-lab-structured-output.json",
        ])
    );
}

#[test]
fn strips_controller_artifact_root_from_lab_offload_command() {
    let input = args(&[
        "homeboy",
        "fuzz",
        "run",
        "--path",
        "/Users/user/Developer/project",
        "--artifact-root",
        "/var/folders/local-homeboy-artifacts",
        "--workload",
        "smoke",
        "--artifact-root=/tmp/also-local",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "fuzz",
            "run",
            "--path",
            "/home/user/Developer/project",
            "--workload",
            "smoke",
        ])
    );
}

#[test]
fn strips_lab_only_flags_from_lab_offload_command() {
    let input = args(&[
        "homeboy",
        "fuzz",
        "run",
        "jetpack",
        "--rig",
        "jetpack-api-route-inventory",
        "--lab-only",
        "--no-local-execution",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/jetpack", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "fuzz",
            "run",
            "jetpack",
            "--rig",
            "jetpack-api-route-inventory",
        ])
    );
}

#[test]
fn strips_controller_artifact_root_from_runner_resident_command() {
    let input = args(&[
        "homeboy",
        "agent-task",
        "status",
        "agent-task-123",
        "--artifact-root",
        "/var/folders/local-homeboy-artifacts",
        "--artifact-root=/tmp/also-local",
    ]);

    assert_eq!(
        rewrite_runner_resident_lab_offload_args(&input, None),
        args(&[
            "homeboy",
            "--force-hot",
            "agent-task",
            "status",
            "agent-task-123",
        ])
    );
}

#[test]
fn leaves_passthrough_path_args_untouched() {
    let input = args(&[
        "homeboy",
        "test",
        "--path=/Users/user/Developer/project",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        "--path",
        "test-fixture",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "test",
            "--path=/home/user/Developer/project",
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
        "sample-plugin",
        "--path",
        "/Users/user/Developer/sample-plugin@fix",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        filter,
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/sample-plugin@fix", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "test",
            "sample-plugin",
            "--path",
            "/home/user/Developer/sample-plugin@fix",
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
        "/Users/user/Developer/project",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "refactor",
            "--from",
            "audit",
            "--path",
            "/home/user/Developer/project",
        ])
    );
}

#[test]
fn detects_lab_offload_source_path_from_path_flag() {
    let input = args(&["homeboy", "test", "--path", "/Users/user/Developer/project"]);

    assert_eq!(
        lab_offload_source_path(&input).expect("path"),
        std::path::PathBuf::from("/Users/user/Developer/project")
    );
}

#[test]
fn rig_check_lab_offload_uses_explicit_component_path_as_source() {
    let input = args(&[
        "homeboy",
        "rig",
        "check",
        "woocommerce-performance",
        "--path",
        "/Users/user/Developer/woocommerce",
        "--runner",
        "homeboy-lab",
        "--lab-only",
    ]);

    assert_eq!(
        lab_offload_source_path(&input).expect("rig check source path"),
        std::path::PathBuf::from("/Users/user/Developer/woocommerce")
    );
    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/woocommerce", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "rig",
            "check",
            "woocommerce-performance",
            "--path",
            "/home/user/Developer/woocommerce",
        ])
    );
}
