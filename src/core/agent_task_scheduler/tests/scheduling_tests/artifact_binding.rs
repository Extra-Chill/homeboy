//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

mod artifact_binding_tests {
    use super::*;

    #[test]
    fn runtime_bundle_artifacts_materialize_required_typed_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("runtime-123");
        let files = bundle.join("files");
        fs::create_dir_all(&files).expect("runtime files");
        let patch_path = files.join("patch.diff");
        let transcript_path = files.join("transcript.json");
        fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
        fs::write(&transcript_path, "{\"events\":[]}").expect("transcript");

        let scheduler = AgentTaskScheduler::new(RuntimeBundleOutcomeExecutor {
            patch_path: patch_path.clone(),
            transcript_path: transcript_path.clone(),
        });
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].expected_artifacts = vec![
            "patch".to_string(),
            "agent_result".to_string(),
            "transcript".to_string(),
        ];

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 1);
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        for name in ["patch", "agent_result", "transcript"] {
            assert!(
                outcome
                    .typed_artifacts
                    .iter()
                    .any(|artifact| artifact.name == name),
                "missing typed artifact {name}"
            );
        }
        let patch = outcome
            .typed_artifacts
            .iter()
            .find(|artifact| artifact.name == "patch")
            .expect("patch typed artifact");
        assert_eq!(
            patch
                .artifact
                .as_ref()
                .and_then(|artifact| artifact.path.as_deref()),
            Some(patch_path.to_str().expect("patch path"))
        );
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.required_typed_artifacts_normalized"
        }));
    }

    #[test]
    fn templates_prior_output_into_downstream_task_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan =
            AgentTaskPlan::new("plan-output-dag", vec![request("idea"), request("design")]);
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.tasks[1].executor.config = json!({
            "github_issue": "{{outputs.issue_number}}",
            "instructions": "Use issue {{ outputs.issue_number }}"
        });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(design.instructions, "Build design for issue #3447");
        assert_eq!(design.executor.config["github_issue"], json!(3447));
        assert_eq!(design.executor.config["instructions"], "Use issue 3447");
        assert_eq!(design.metadata["generated_from_outputs"], json!(true));
        assert_eq!(
            design.metadata["resolved_output_bindings"]["issue_number"],
            json!(3447)
        );
        let idea_succeeded_index = aggregate
            .events
            .iter()
            .position(|event| event.task_id == "idea" && event.state == AgentTaskState::Succeeded)
            .expect("idea succeeded event");
        let design_running_index = aggregate
            .events
            .iter()
            .position(|event| event.task_id == "design" && event.state == AgentTaskState::Running)
            .expect("design running event");
        assert!(idea_succeeded_index < design_running_index);
    }

    #[test]
    fn binds_typed_artifact_payload_into_downstream_task_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-dag",
            vec![request("idea"), request("design")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[1].inputs = json!({ "packet": "{{outputs.concept_packet}}" });
        plan.artifact_outputs.insert(
            "idea".to_string(),
            vec![AgentTaskArtifactOutputDeclaration {
                name: "concept_packet".to_string(),
                kind: "concept_packet".to_string(),
                schema: Some("example/concept-packet/v1".to_string()),
                artifact_id: None,
                payload_path: Some("/title".to_string()),
            }],
        );
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("example/concept-packet/v1".to_string()),
                            artifact_id: Some("concept".to_string()),
                            payload_path: Some("/title".to_string()),
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(design.inputs["packet"], json!("Demo concept"));
        assert_eq!(aggregate.artifact_lineage.len(), 1);
        assert_eq!(aggregate.artifact_lineage[0].name, "concept_packet");
        assert_eq!(aggregate.artifact_lineage[0].payload, json!("Demo concept"));
    }

    #[test]
    fn required_concept_packet_binding_uses_canonical_typed_artifact() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler =
            crate::core::agent_task_scheduler::AgentTaskScheduler::new(ConceptPacketExecutor {
                observed: Arc::clone(&observed),
                emit_concept_packet: true,
            });
        let mut plan = AgentTaskPlan::new(
            "plan-concept-packet-typed-artifact",
            vec![request("idea"), request("build")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[0].artifact_declarations = vec![concept_packet_declaration()];
        plan.tasks[1].inputs = json!({ "concept_packet": "{{outputs.concept_packet}}" });
        plan.output_dependencies.insert(
            "build".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let build = observed
            .iter()
            .find(|request| request.task_id == "build")
            .expect("build request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(build.inputs["concept_packet"]["title"], "Typed concept");
    }

    #[test]
    fn required_concept_packet_binding_fails_without_canonical_typed_artifact() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler =
            crate::core::agent_task_scheduler::AgentTaskScheduler::new(ConceptPacketExecutor {
                observed: Arc::clone(&observed),
                emit_concept_packet: false,
            });
        let mut plan = AgentTaskPlan::new(
            "plan-concept-packet-missing-typed-artifact",
            vec![request("idea"), request("build")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[0].artifact_declarations = vec![concept_packet_declaration()];
        plan.output_dependencies.insert(
            "build".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(aggregate.totals.skipped, 1);
        assert_eq!(aggregate.totals.succeeded, 0);
        assert!(observed.iter().all(|request| request.task_id != "build"));
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "build"
                && event.state == AgentTaskState::Skipped
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("required artifact binding"))
        }));
        let idea = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "idea")
            .expect("idea outcome");
        assert!(idea.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.required_typed_artifacts_missing"
                && diagnostic.message.contains("concept_packet")
        }));
        let build = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "build")
            .expect("build skipped outcome");
        assert!(build.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required artifact binding")
                && diagnostic.message.contains("concept_packet")
        }));
        assert_eq!(build.status, AgentTaskOutcomeStatus::Failed);
    }

    #[test]
    fn binds_artifacts_to_generic_child_run_ids_for_durable_fanout() {
        let scheduler = AgentTaskScheduler::new(GenericChildRunExecutor);
        let mut plan = AgentTaskPlan::new(
            "fuzz/campaign-1",
            vec![request("case-a"), request("case-b")],
        );
        plan.options.max_concurrency = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.child_runs.len(), 2);
        let mut child_runs = aggregate.child_runs.clone();
        child_runs.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        assert_eq!(child_runs[0].task_id, "case-a");
        assert_eq!(child_runs[0].run_id, "child-case-a");
        assert_eq!(child_runs[0].provider.as_deref(), Some("generic-fuzz"));
        assert_eq!(child_runs[0].state, AgentTaskState::Succeeded);
        assert_eq!(aggregate.artifact_bindings.len(), 2);
        let mut artifact_bindings = aggregate.artifact_bindings.clone();
        artifact_bindings.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        assert_eq!(artifact_bindings[0].task_id, "case-a");
        assert_eq!(artifact_bindings[0].run_id, "child-case-a");
        assert_eq!(artifact_bindings[0].artifact_id, "artifact-case-a");
        assert_eq!(artifact_bindings[0].kind, "fuzz-report");
        assert_eq!(
            artifact_bindings[0].path.as_deref(),
            Some("artifacts/case-a/report.json")
        );
    }

    #[test]
    fn skips_required_typed_artifact_binding_when_artifact_is_missing() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-skip",
            vec![request("idea"), request("design")],
        );
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "finding_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "finding_packet".to_string(),
                            schema: None,
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
        );
        assert!(observed.iter().all(|request| request.task_id != "design"));
        let skipped = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "design")
            .expect("skipped outcome");
        assert!(skipped.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required artifact binding")
        }));
    }

    #[test]
    fn optional_typed_artifact_binding_uses_default() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-default",
            vec![request("idea"), request("design")],
        );
        plan.tasks[1].inputs = json!({ "packet": "{{outputs.finding_packet}}" });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "finding_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "finding_packet".to_string(),
                            schema: None,
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: false,
                        default: json!({ "findings": [] }),
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(design.inputs["packet"], json!({ "findings": [] }));
    }

    #[test]
    fn skips_downstream_task_when_required_output_is_missing() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan =
            AgentTaskPlan::new("plan-output-skip", vec![request("idea"), request("design")]);
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
        );
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(aggregate.totals.skipped, 1);
        assert_eq!(aggregate.totals.failed, 0);
        assert!(observed.iter().all(|request| request.task_id != "design"));
        assert!(aggregate
            .events
            .iter()
            .any(|event| { event.task_id == "design" && event.state == AgentTaskState::Skipped }));
        let skipped = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "design")
            .expect("skipped outcome");
        assert_eq!(skipped.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            skipped.failure_classification,
            Some(AgentTaskFailureClassification::InvalidInput)
        );
        assert!(skipped.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required output binding")
        }));
    }
}
