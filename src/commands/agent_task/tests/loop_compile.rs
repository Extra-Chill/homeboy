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
fn compile_loop_command_propagates_runtime_provider_model_options_into_runtime_task_input() {
    let temp = tempfile::tempdir().expect("tempdir");
    let definition_path = temp.path().join("repo-loop-runtime-opts.json");
    std::fs::write(
        &definition_path,
        serde_json::to_string(&json!({
            "schema": "wpsg/loop-spec/v1",
            "loop_id": "wpsg/runtime-opts-loop",
            "metadata": {
                "dispatch_defaults": {
                    "backend": "fixture",
                    // Top-level `model` is intentionally distinct from the
                    // provider_config `model` so the assertions below prove the
                    // production precedence rule (top-level wins) rather than a
                    // coincidence of identical fixture values.
                    "model": "gpt-cli",
                    "provider_config": json!({
                        "provider": "codex",
                        "model": "gpt-config",
                        "options": { "reasoning_effort": "high" }
                    })
                    .to_string()
                }
            },
            "workflows": [
                {
                    "workflow_id": "store-idea",
                    "prompt": "Generate a concept packet.",
                    "runtime_execution": {
                        "kind": "bundle",
                        "ability": "runtime-package/run",
                        "input": {
                            "package": { "source": "bundles/store-idea-agent" }
                        }
                    }
                },
                {
                    // Control workflow whose runtime_task.input already declares
                    // its own provider/model. Production uses `or_insert`, so the
                    // explicit values MUST survive untouched — proving the
                    // propagation does not clobber caller-provided selection.
                    "workflow_id": "explicit-selection",
                    "prompt": "Run with an explicit provider.",
                    "runtime_execution": {
                        "kind": "bundle",
                        "ability": "runtime-package/run",
                        "input": {
                            "package": { "source": "bundles/explicit-agent" },
                            "provider": "anthropic",
                            "model": "claude-explicit"
                        }
                    }
                }
            ]
        }))
        .expect("definition json"),
    )
    .expect("write definition");

    let (value, status) = super::super::loop_definition::compile_loop(CompileLoopArgs {
        definition: format!("@{}", definition_path.display()),
    })
    .expect("compile loop");

    assert_eq!(status, 0);
    let runtime_task = &value["tasks"][0]["inputs"]["runtime_task"];
    assert_eq!(runtime_task["ability"], "runtime-package/run");
    assert_eq!(
        runtime_task["input"]["package"]["source"],
        "bundles/store-idea-agent"
    );
    // CLI/provider runtime selection must be propagated into runtime_task.input.
    assert_eq!(runtime_task["input"]["provider"], "codex");
    // Precedence: the top-level `model` default is inserted before the
    // provider_config block, so `gpt-cli` (top-level) wins over `gpt-config`
    // (provider_config). This asserts the real ordering logic in
    // `apply_runtime_task_dispatch_defaults`, not just presence of a value.
    assert_eq!(runtime_task["input"]["model"], "gpt-cli");
    assert_ne!(runtime_task["input"]["model"], "gpt-config");
    assert_eq!(runtime_task["input"]["options"]["reasoning_effort"], "high");

    // Control: a workflow that already declares its own provider/model keeps
    // them — propagation is additive (`or_insert`), never overwriting.
    let explicit = &value["tasks"][1]["inputs"]["runtime_task"];
    assert_eq!(explicit["input"]["provider"], "anthropic");
    assert_eq!(explicit["input"]["model"], "claude-explicit");
    // Options were not declared on this workflow, so the dispatch default still
    // fills them in.
    assert_eq!(explicit["input"]["options"]["reasoning_effort"], "high");
}

#[test]
fn compile_loop_command_omits_runtime_options_without_dispatch_defaults() {
    // Negative control for the propagation behavior above: a spec with NO
    // `dispatch_defaults` must NOT have provider/model/options synthesized onto
    // the generated runtime_task.input. This proves the asserted values in the
    // positive test originate from production propagation rather than from the
    // bundle fixture or the runtime_execution block itself.
    let temp = tempfile::tempdir().expect("tempdir");
    let definition_path = temp.path().join("repo-loop-no-runtime-opts.json");
    std::fs::write(
        &definition_path,
        serde_json::to_string(&json!({
            "schema": "wpsg/loop-spec/v1",
            "loop_id": "wpsg/no-runtime-opts-loop",
            "metadata": {
                "dispatch_defaults": { "backend": "fixture" }
            },
            "workflows": [
                {
                    "workflow_id": "store-idea",
                    "prompt": "Generate a concept packet.",
                    "runtime_execution": {
                        "kind": "bundle",
                        "ability": "runtime-package/run",
                        "input": {
                            "package": { "source": "bundles/store-idea-agent" }
                        }
                    }
                }
            ]
        }))
        .expect("definition json"),
    )
    .expect("write definition");

    let (value, status) = super::super::loop_definition::compile_loop(CompileLoopArgs {
        definition: format!("@{}", definition_path.display()),
    })
    .expect("compile loop");

    assert_eq!(status, 0);
    let runtime_task = &value["tasks"][0]["inputs"]["runtime_task"];
    assert_eq!(runtime_task["ability"], "runtime-package/run");
    assert_eq!(
        runtime_task["input"]["package"]["source"],
        "bundles/store-idea-agent"
    );
    // No provider/model/options metadata existed to propagate, so these keys
    // must be absent on the generated input.
    assert!(runtime_task["input"].get("provider").is_none());
    assert!(runtime_task["input"].get("model").is_none());
    assert!(runtime_task["input"].get("options").is_none());
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
fn compile_loop_command_rejects_controller_workflow_gates_and_metrics() {
    let error = loop_definition::compile_loop(CompileLoopArgs {
        definition: serde_json::to_string(&json!({
            "loop_id": "repo-loop-with-controller-gates",
            "gates": [{ "gate_id": "review" }],
            "metrics": [{ "metric_id": "fallback_blocks" }],
            "workflows": [
                {
                    "workflow_id": "publish",
                    "prompt": "Publish the output.",
                    "gates": ["review"],
                    "metrics": ["fallback_blocks"]
                }
            ]
        }))
        .expect("definition json"),
    })
    .expect_err("controller workflow gates and metrics are rejected");

    assert!(error.message.contains("controller-only sections"));
    let diagnostics = error.details["tried"].as_array().expect("diagnostics");
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        .as_str()
        .unwrap_or_default()
        .contains("workflows[publish].gates")));
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        .as_str()
        .unwrap_or_default()
        .contains("workflows[publish].metrics")));
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

    let (value, status) =
        super::support::controller_materialize(AgentTaskControllerMaterializeArgs {
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
        value["schema"],
        "homeboy/agent-task-loop-spec-materialization/v1"
    );
    assert_eq!(
        value["spec"]["workflows"][0]["inputs"]["policy_inputs"]["example-policy"]
            ["requested_tier"],
        "foundation"
    );
    // The second workflow declared no `inputs` block of its own; materialization
    // must synthesize one and project every policy's results into it.
    assert_eq!(
        value["spec"]["workflows"][1]["inputs"]["policy_results"]["example-policy"]["decision"],
        "hold"
    );
    assert_eq!(
        value["spec"]["workflows"][1]["inputs"]["policy_results"]["example-policy"]
            ["selected_tier"],
        "foundation"
    );
    assert_eq!(
        value["spec"]["workflows"][1]["inputs"]["policy_results"]["second-policy"]["enabled"],
        true
    );
    // Provenance for each policy is recorded under spec metadata keyed by policy id.
    assert_eq!(
        value["spec"]["metadata"]["policy_materialization"]["example-policy"]["provenance"]
            ["source"],
        "fixture"
    );
    assert_eq!(
        value["spec"]["metadata"]["policy_materialization"]["example-policy"]["provenance"]
            ["sha256"],
        "abc123"
    );
    assert_eq!(
        value["spec"]["metadata"]["policy_materialization"]["second-policy"]["provenance"]
            ["source"],
        "second-fixture"
    );
    // Policies without `policy_inputs` must not leak an `example-policy`-style block
    // onto the first workflow for the second policy id.
    assert!(value["spec"]["workflows"][0]["inputs"]["policy_inputs"]
        .get("second-policy")
        .is_none());
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

    let error = super::support::controller_materialize(AgentTaskControllerMaterializeArgs {
        spec: format!("@{}", spec_path.display()),
        inputs: None,
        policy_results: vec![format!("@{}", policy_path.display())],
    })
    .expect_err("policy result fields are validated");

    assert!(error.message.contains("policy materialization fields"));
    // The validation must reject the non-object field as an invalid-argument error
    // scoped to the `policy_results` field and attribute it to the offending policy id.
    assert_eq!(error.details["field"], "policy-result.policy_results");
    assert_eq!(error.details["id"], "example-policy");
    assert!(error.message.contains("must be JSON objects when present"));
}

#[test]
fn controller_materialize_runs_generator_manifest_and_records_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest_path = temp.path().join("generator.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&json!({
            "schema": "homeboy/agent-task-loop-spec-generator/v1",
            "command": [
                "/bin/sh",
                "-c",
                "cat > generated-loop.json <<'JSON'\n{\"loop_id\":\"generated-materialize-loop\",\"workflows\":[{\"workflow_id\":\"brief\",\"prompt\":\"Draft.\"}]}\nJSON"
            ],
            "inputs": { "idea": "evidence" },
            "output_path": "generated-loop.json"
        }))
        .expect("manifest json"),
    )
    .expect("write manifest");

    let (value, status) =
        super::support::controller_materialize(AgentTaskControllerMaterializeArgs {
            spec: format!("@{}", manifest_path.display()),
            inputs: None,
            policy_results: Vec::new(),
        })
        .expect("materialize generated spec");

    assert_eq!(status, 0);
    assert_eq!(value["spec"]["loop_id"], "generated-materialize-loop");
    assert_eq!(
        value["generator_evidence"]["schema"],
        "homeboy/agent-task-loop-spec-generator-evidence/v1"
    );
    assert_eq!(value["generator_evidence"]["command"][0], "/bin/sh");
    assert_eq!(value["generator_evidence"]["inputs"]["idea"], "evidence");
    assert!(value["generator_evidence"]["output_path"]
        .as_str()
        .expect("output path")
        .ends_with("generated-loop.json"));
    assert_eq!(
        value["generator_evidence"]["validation_result"]["valid"],
        true
    );
    assert_eq!(
        value["generator_evidence"]["spec_hash"]
            .as_str()
            .expect("hash")
            .len(),
        64
    );
}

#[test]
fn controller_materialize_caps_generator_output_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest_path = temp.path().join("generator.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&json!({
            "schema": "homeboy/agent-task-loop-spec-generator/v1",
            "command": [
                "/bin/sh",
                "-c",
                "printf 1234567890abcdef; cat > generated-loop.json <<'JSON'\n{\"loop_id\":\"generated-output-cap-loop\",\"workflows\":[{\"workflow_id\":\"brief\",\"prompt\":\"Draft.\"}]}\nJSON"
            ],
            "output_path": "generated-loop.json",
            "max_stdout_bytes": 12,
            "max_stderr_bytes": 12
        }))
        .expect("manifest json"),
    )
    .expect("write manifest");

    let (value, status) =
        super::support::controller_materialize(AgentTaskControllerMaterializeArgs {
            spec: format!("@{}", manifest_path.display()),
            inputs: None,
            policy_results: Vec::new(),
        })
        .expect("materialize generated spec");

    assert_eq!(status, 0);
    assert_eq!(value["spec"]["loop_id"], "generated-output-cap-loop");
    assert_eq!(
        value["generator_evidence"]["status"]["stdout"],
        "1234567890ab"
    );
    assert_eq!(
        value["generator_evidence"]["status"]["stdout_truncated"],
        true
    );
    assert_eq!(
        value["generator_evidence"]["status"]["stdout_bytes_read"],
        16
    );
    assert_eq!(
        value["generator_evidence"]["status"]["max_stdout_bytes"],
        12
    );
}

#[test]
fn controller_materialize_reports_generator_timeout_diagnostics() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest_path = temp.path().join("generator.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&json!({
            "schema": "homeboy/agent-task-loop-spec-generator/v1",
            "command": ["/bin/sh", "-c", "sleep 2"],
            "output_path": "generated-loop.json",
            "timeout_seconds": 1
        }))
        .expect("manifest json"),
    )
    .expect("write manifest");

    let error = super::support::controller_materialize(AgentTaskControllerMaterializeArgs {
        spec: format!("@{}", manifest_path.display()),
        inputs: None,
        policy_results: Vec::new(),
    })
    .expect_err("timeout is rejected");

    assert_eq!(error.details["field"], "spec.command");
    assert!(error.message.contains("timed out after 1 seconds"));
    assert_eq!(
        error.details["diagnostics"][0]["class"],
        "generator.timeout"
    );
    assert_eq!(
        error.details["diagnostics"][0]["data"]["timeout_seconds"],
        1
    );
    assert_eq!(error.details["diagnostics"][0]["data"]["timed_out"], true);
    assert!(error.details["diagnostics"][0]["data"]["cwd"]
        .as_str()
        .expect("cwd")
        .contains(temp.path().to_str().expect("temp path")));
}

#[test]
fn controller_materialize_reports_missing_generated_spec_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest_path = temp.path().join("generator.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&json!({
            "schema": "homeboy/agent-task-loop-spec-generator/v1",
            "command": ["/bin/sh", "-c", "true"],
            "output_path": "missing-loop.json"
        }))
        .expect("manifest json"),
    )
    .expect("write manifest");

    let error = super::support::controller_materialize(AgentTaskControllerMaterializeArgs {
        spec: format!("@{}", manifest_path.display()),
        inputs: None,
        policy_results: Vec::new(),
    })
    .expect_err("missing generated output is rejected");

    assert_eq!(error.details["field"], "spec.output_path");
    assert!(error.message.contains("generated spec was not found"));
    assert!(error.message.contains("missing-loop.json"));
    assert!(error.details["tried"][0]
        .as_str()
        .expect("remediation")
        .contains("must write missing-loop.json"));
}

#[test]
fn controller_from_spec_doctor_reports_missing_provider_before_resume() {
    let (value, status) = controller_from_spec(AgentTaskControllerFromSpecArgs {
        spec: serde_json::to_string(&json!({
            "loop_id": "doctor-missing-provider-loop",
            "workflows": [
                { "workflow_id": "brief", "prompt": "Draft the brief." }
            ]
        }))
        .expect("spec json"),
        resume: false,
        inputs: None,
        policy_results: Vec::new(),
        max_actions: None,
        reconcile_stale: false,
        replace: false,
        fork: false,
        resume_existing: false,
        doctor: true,
        dispatch: AgentTaskControllerDispatchArgs {
            dispatch_backend: Some("missing-provider".to_string()),
            dispatch_selector: None,
            dispatch_model: None,
            dispatch_provider_config: None,
        },
    })
    .expect("doctor report");

    assert_eq!(status, 1);
    assert_eq!(
        value["schema"],
        "homeboy/agent-task-loop-controller-doctor-result/v1"
    );
    assert_eq!(value["ok"], false);
    assert!(value["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .any(|check| check["id"]
            .as_str()
            .unwrap_or_default()
            .ends_with(".provider")
            && check["status"] == "error"
            && check["message"]
                .as_str()
                .unwrap_or_default()
                .contains("missing-provider")));
}

#[test]
fn controller_from_spec_doctor_accepts_fixture_provider() {
    let (value, status) = controller_from_spec(AgentTaskControllerFromSpecArgs {
        spec: serde_json::to_string(&json!({
            "loop_id": "doctor-fixture-provider-loop",
            "workflows": [
                { "workflow_id": "brief", "prompt": "Draft the brief." }
            ]
        }))
        .expect("spec json"),
        resume: false,
        inputs: None,
        policy_results: Vec::new(),
        max_actions: None,
        reconcile_stale: false,
        replace: false,
        fork: false,
        resume_existing: false,
        doctor: true,
        dispatch: AgentTaskControllerDispatchArgs {
            dispatch_backend: Some("fixture".to_string()),
            dispatch_selector: None,
            dispatch_model: None,
            dispatch_provider_config: None,
        },
    })
    .expect("doctor report");

    assert_eq!(status, 0);
    assert_eq!(value["ok"], true);
    assert!(value["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .any(|check| check["message"]
            == "Fixture provider is available for deterministic local execution"));
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
