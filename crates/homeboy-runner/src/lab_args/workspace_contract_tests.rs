use super::{lab_offload_source_path, rewrite_lab_offload_args, LabPathRemap};

#[test]
fn lab_source_path_uses_agent_task_dispatch_workspace() {
    let workspace = tempfile::tempdir().expect("workspace");
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "dispatch".to_string(),
        "--workspace".to_string(),
        workspace.path().display().to_string(),
        "--prompt".to_string(),
        "cook".to_string(),
    ];

    assert_eq!(
        lab_offload_source_path(&args).expect("source path"),
        workspace.path()
    );
}

#[test]
fn rewrite_remaps_agent_task_workspace_path() {
    let input = [
        "homeboy",
        "agent-task",
        "cook",
        "--workspace",
        "/Users/user/project",
        "--workspace=/Users/user/project/nested",
    ]
    .map(str::to_string)
    .to_vec();
    let mappings = vec![LabPathRemap {
        local: "/Users/user/project".to_string(),
        remote: "/runner/project".to_string(),
    }];

    assert_eq!(
        rewrite_lab_offload_args(&input, "/runner/project", &mappings, None),
        [
            "homeboy",
            "agent-task",
            "cook",
            "--workspace",
            "/runner/project",
            "--workspace=/runner/project/nested",
        ]
        .map(str::to_string)
        .to_vec()
    );
}
