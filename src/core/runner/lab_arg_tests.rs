use super::super::lab_args::{
    lab_offload_source_path, rewrite_lab_offload_args, EXPLICIT_PASSTHROUGH_SENTINEL,
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
