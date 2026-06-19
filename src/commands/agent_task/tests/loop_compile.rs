//! Agent-task command loop-spec compilation and controller materialization tests.

use super::support::*;

#[test]
fn compile_loop_command_emits_agent_task_plan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let definition_path = temp.path().join("loop-definition.json");
    std::fs::write(
        &definition_path,
        serde_json::to_string(&json!({
            "schema": "homeboy/agent-task-loop-definition/v1",
            "loop_id": "cli/loop",
            "tasks": [
                { "task_id": "idea", "request": agent_task_request_json("idea") },
                {
                    "task_id": "design",
                    "request": agent_task_request_json("design"),
                    "depends_on": ["idea"],
                    "bindings": {
                        "concept_packet": { "task_id": "idea", "path": "/outputs/concept_packet" }
                    }
                }
            ]
        }))
        .expect("definition json"),
    )
    .expect("write definition");

    let (value, status) = loop_definition::compile_loop(CompileLoopArgs {
        definition: format!("@{}", definition_path.display()),
    })
    .expect("compile loop");

    assert_eq!(status, 0);
    assert_eq!(value["schema"], "homeboy/agent-task-plan/v1");
    assert_eq!(value["plan_id"], "cli/loop");
    assert_eq!(value["tasks"].as_array().expect("tasks").len(), 2);
    assert_eq!(
        value["output_dependencies"]["design"]["bindings"]["concept_packet"]["task_id"],
        "idea"
    );
}

#[test]
fn compile_loop_command_emits_plan_from_repo_loop_spec() {
    let temp = tempfile::tempdir().expect("tempdir");
    let definition_path = temp.path().join("repo-loop.json");
    std::fs::write(
        &definition_path,
        serde_json::to_string(&json!({
            "schema": "wpsg/loop-spec/v1",
            "loop_id": "wpsg/site-loop",
            "metadata": {
                "group_key": "wpsg-site",
                "dispatch_defaults": {
                    "backend": "fixture",
                    "selector": "local",
                    "cwd": temp.path().display().to_string(),
                    "repo": "wp-site-generator@fixture"
                }
            },
            "agents": [
                { "agent_id": "builder", "tools": ["write-file"], "abilities": ["render-blocks"] }
            ],
            "artifacts": [
                { "artifact_id": "site_brief", "kind": "wpsg/SiteBrief/v1", "required": true },
                { "artifact_id": "theme_patch", "kind": "homeboy/Patch/v1", "required": true }
            ],
            "workflows": [
                {
                    "workflow_id": "brief",
                    "agent_id": "builder",
                    "prompt": "Draft the site brief.",
                    "emits": ["site_brief"]
                },
                {
                    "workflow_id": "build",
                    "prompt": "Build from the site brief.",
                    "consumes": ["site_brief"],
                    "emits": ["theme_patch"]
                }
            ]
        }))
        .expect("definition json"),
    )
    .expect("write definition");

    let (value, status) = loop_definition::compile_loop(CompileLoopArgs {
        definition: format!("@{}", definition_path.display()),
    })
    .expect("compile loop");

    assert_eq!(status, 0);
    assert_eq!(value["schema"], "homeboy/agent-task-plan/v1");
    assert_eq!(value["plan_id"], "wpsg/site-loop");
    assert_eq!(value["group_key"], "wpsg-site");
    assert_eq!(value["tasks"][0]["task_id"], "brief");
    assert_eq!(value["tasks"][0]["executor"]["backend"], "fixture");
    assert_eq!(
        value["tasks"][0]["executor"]["required_capabilities"],
        json!(null)
    );
    assert_eq!(
        value["tasks"][0]["workspace"]["slug"],
        "wp-site-generator@fixture"
    );
    assert_eq!(
        value["output_dependencies"]["build"]["depends_on"],
        json!(["brief"])
    );
    assert_eq!(
        value["output_dependencies"]["build"]["bindings"]["site_brief"]["task_id"],
        "brief"
    );
    assert_eq!(
        value["artifact_outputs"]["brief"][0]["kind"],
        "wpsg/SiteBrief/v1"
    );
}

#[test]
fn compile_loop_command_rejects_controller_only_sections() {
    let error = loop_definition::compile_loop(CompileLoopArgs {
        definition: serde_json::to_string(&json!({
            "loop_id": "repo-loop-with-controller-policy",
            "workflows": [
                { "workflow_id": "brief", "prompt": "Draft the site brief." }
            ],
            "policy": { "policy_id": "runtime-policy", "transitions": [] }
        }))
        .expect("definition json"),
    })
    .expect_err("controller-only section is rejected");

    assert!(error.message.contains("controller-only sections"));
    assert!(error.details["tried"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .any(|diagnostic| diagnostic.as_str().unwrap_or_default().contains("policy")));
}

#[test]
fn controller_materialize_merges_inputs_and_metadata_without_mutating_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_path = temp.path().join("repo-loop.json");
    let inputs_path = temp.path().join("inputs.json");
    std::fs::write(
        &spec_path,
        serde_json::to_string(&json!({
            "loop_id": "materialize-loop",
            "metadata": { "source": "fixture" },
            "artifacts": {
                "brief": { "kind": "example/Brief/v1" }
            },
            "workflows": [
                {
                    "workflow_id": "brief",
                    "prompt": "Draft the brief.",
                    "inputs": { "topic": "existing" },
                    "emits": ["brief"]
                }
            ]
        }))
        .expect("spec json"),
    )
    .expect("write spec");
    std::fs::write(
        &inputs_path,
        serde_json::to_string(&json!({
            "inputs": { "topic": "explicit", "audience": "operators" },
            "metadata": { "run_id": "run-123" }
        }))
        .expect("inputs json"),
    )
    .expect("write inputs");

    let (value, status) = controller_materialize(AgentTaskControllerMaterializeArgs {
        spec: format!("@{}", spec_path.display()),
        inputs: Some(format!("@{}", inputs_path.display())),
        policy_results: Vec::new(),
    })
    .expect("materialize spec");

    assert_eq!(status, 0);
    assert_eq!(
        value["schema"],
        "homeboy/agent-task-loop-spec-materialization/v1"
    );
    assert_eq!(value["spec"]["workflows"][0]["inputs"]["topic"], "explicit");
    assert_eq!(
        value["spec"]["workflows"][0]["inputs"]["audience"],
        "operators"
    );
    assert_eq!(value["spec"]["metadata"]["source"], "fixture");
    assert_eq!(value["spec"]["metadata"]["run_id"], "run-123");

    let source_after: Value = serde_json::from_str(
        &std::fs::read_to_string(&spec_path).expect("source spec remains readable"),
    )
    .expect("source spec json");
    assert_eq!(source_after["workflows"][0]["inputs"]["topic"], "existing");
    assert!(source_after["metadata"].get("run_id").is_none());
}

#[test]
fn controller_materialize_projects_policy_results_with_provenance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_path = temp.path().join("repo-loop.json");
    let policy_path = temp.path().join("policy-result.json");
    let second_policy_path = temp.path().join("second-policy-result.json");
    std::fs::write(
        &spec_path,
        serde_json::to_string(&json!({
            "loop_id": "materialize-policy-loop",
            "workflows": [
                {
                    "workflow_id": "brief",
                    "prompt": "Draft the brief.",
                    "inputs": { "topic": "existing" }
                },
                {
                    "workflow_id": "build",
                    "prompt": "Build the site."
                }
            ]
        }))
        .expect("spec json"),
    )
    .expect("write spec");
    std::fs::write(
        &policy_path,
        serde_json::to_string(&json!({
            "policy_id": "example-policy",
            "policy_inputs": { "requested_tier": "foundation" },
            "policy_results": { "selected_tier": "foundation", "decision": "hold" },
            "provenance": { "source": "fixture", "sha256": "abc123" }
        }))
        .expect("policy result json"),
    )
    .expect("write policy result");
    std::fs::write(
        &second_policy_path,
        serde_json::to_string(&json!({
            "policy_id": "second-policy",
            "policy_results": { "enabled": true },
            "provenance": { "source": "second-fixture" }
        }))
        .expect("policy result json"),
    )
    .expect("write second policy result");

    let (value, status) = controller_materialize(AgentTaskControllerMaterializeArgs {
        spec: format!("@{}", spec_path.display()),
        inputs: None,
        policy_results: vec![
            format!("@{}", policy_path.display()),
            format!("@{}", second_policy_path.display()),
        ],
    })
    .expect("materialize spec");

    assert_eq!(status, 0);
    assert_eq!(
        value["spec"]["workflows"][0]["inputs"]["policy_inputs"]["example-policy"]
            ["requested_tier"],
        "foundation"
    );
    assert_eq!(
        value["spec"]["workflows"][1]["inputs"]["policy_results"]["example-policy"]["decision"],
        "hold"
    );
    assert_eq!(
        value["spec"]["workflows"][1]["inputs"]["policy_results"]["second-policy"]["enabled"],
        true
    );
    assert_eq!(
        value["spec"]["metadata"]["policy_materialization"]["example-policy"]["provenance"]
            ["source"],
        "fixture"
    );
    assert_eq!(
        value["spec"]["metadata"]["policy_materialization"]["second-policy"]["provenance"]
            ["source"],
        "second-fixture"
    );
}

#[test]
fn controller_materialize_rejects_non_object_policy_result_fields() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_path = temp.path().join("repo-loop.json");
    let policy_path = temp.path().join("policy-result.json");
    std::fs::write(
        &spec_path,
        serde_json::to_string(&json!({
            "loop_id": "materialize-policy-validation-loop",
            "workflows": [{ "workflow_id": "brief", "prompt": "Draft." }]
        }))
        .expect("spec json"),
    )
    .expect("write spec");
    std::fs::write(
        &policy_path,
        serde_json::to_string(&json!({
            "policy_id": "example-policy",
            "policy_results": "hold"
        }))
        .expect("policy result json"),
    )
    .expect("write policy result");

    let error = controller_materialize(AgentTaskControllerMaterializeArgs {
        spec: format!("@{}", spec_path.display()),
        inputs: None,
        policy_results: vec![format!("@{}", policy_path.display())],
    })
    .expect_err("policy result fields are validated");

    assert!(error.message.contains("policy materialization fields"));
}

#[test]
fn compile_loop_command_rejects_undeclared_workflow_artifacts() {
    let error = loop_definition::compile_loop(CompileLoopArgs {
        definition: serde_json::to_string(&json!({
            "loop_id": "repo-loop-with-missing-artifact",
            "artifacts": {
                "brief": { "kind": "example/Brief/v1" }
            },
            "workflows": [
                { "workflow_id": "brief", "prompt": "Draft.", "emits": ["missing"] }
            ]
        }))
        .expect("definition json"),
    })
    .expect_err("undeclared artifact is rejected");

    assert!(error.message.contains("artifacts"));
    assert!(error.details["tried"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .any(|diagnostic| diagnostic
            .as_str()
            .unwrap_or_default()
            .contains("references undeclared artifact 'missing'")));
}
