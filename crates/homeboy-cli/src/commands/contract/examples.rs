//! Contract example/fixture builders.
//!
//! Constructs the canonical JSON `Value` examples embedded in the contract
//! schema catalog (artifact manifests, materialization plans, runner execution
//! envelopes, lab workload/handoff, lifecycle indexes, …). Extracted from the
//! `contract` command module to keep the command/validation logic separate from
//! the large body of static example data. Inherits the parent module's schema
//! constant imports via `use super::*`.

use super::*;

pub(super) fn artifact_manifest_example() -> Value {
    json!({
        "schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
        "artifacts": [
            {
                "id": "transcript",
                "path": crate::command_contract::RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH,
                "kind": "transcript",
                "label": "Runtime transcript",
                "content_type": "application/json",
                "metadata": {}
            },
            {
                "id": "final_output",
                "path": crate::command_contract::RUNTIME_AGENT_FINAL_OUTPUT_ARTIFACT_PATH,
                "kind": "final_output",
                "label": "Final runtime output",
                "content_type": "text/markdown",
                "metadata": {}
            }
        ]
    })
}

pub(super) fn path_materialization_plan_example() -> Value {
    json!({
        "schema": PATH_MATERIALIZATION_PLAN_SCHEMA,
        "entries": [
            {
                "role": "primary_workspace",
                "owner": "runner_exec.source_snapshot",
                "local_path": "/local/project",
                "remote_path": "/runner/project",
                "materialization_mode": "snapshot",
                "validation_status": "materialized"
            },
            {
                "role": "required_path",
                "owner": "runner_exec.require_paths",
                "remote_path": "/runner/cache",
                "materialization_mode": "existing_remote",
                "validation_status": "validated"
            }
        ]
    })
}

pub(super) fn runner_artifact_manifest_ref_example() -> Value {
    json!({
        "schema": RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
        "manifest_schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
        "path": runner_artifact_manifest_example_path()
    })
}

pub(super) fn runner_execution_envelope_example() -> Value {
    json!({
        "schema": RUNNER_EXECUTION_ENVELOPE_SCHEMA,
        "envelope_id": "execution-1",
        "source": { "kind": "agent_task", "ref_id": "task-1" },
        "artifact_declarations": [
            {
                "name": "transcript",
                "type": "file",
                "path": crate::command_contract::RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH,
                "required": false,
                "metadata": {}
            }
        ],
        "result_refs": { "task_id": "task-1", "artifacts": [] },
        "metadata": {}
    })
}

pub(super) fn artifact_postprocess_example() -> Value {
    json!({
        "schema": ARTIFACT_POSTPROCESS_SCHEMA,
        "plan_id": "artifact-postprocess-1",
        "artifact_roots": [
            {
                "id": "primary",
                "path": "artifacts",
                "persisted_ref": "runner-artifact://run-1/artifacts",
                "manifest_path": RUNNER_ARTIFACT_MANIFEST_FILE
            }
        ],
        "actions": [
            {
                "id": "summarize",
                "helper": "artifact-helper",
                "action": "summarize",
                "input": "artifacts/input.json",
                "output": "artifact-postprocess/summary.json",
                "parameters": {},
                "required": true
            }
        ],
        "reviewer_refs": [],
        "metadata": {}
    })
}

pub(super) fn lab_runner_workload_example() -> Value {
    json!({
        "schema": LAB_RUNNER_WORKLOAD_SCHEMA,
        "workload_id": "workload-1",
        "kind": { "command_label": "test", "command_family": "quality" },
        "workspace_mappings": {
            "source_path_mode": "cwd_or_path_flag",
            "workspace_mode_policy": "git",
            "mapping_ref": "workspace-1"
        },
        "required_capabilities": [{ "name": "cargo", "required": true }],
        "required_secrets": { "categories": [] },
        "required_extensions": [],
        "required_extension_revisions": [],
        "mutation_policy": {
            "capture_patch": false,
            "mutation_flag": null,
            "allow_dirty_lab_workspace": false
        },
        "assignment": { "runner_id": "runner-1", "runner_mode": "lab", "source": "selected" },
        "state": { "status": "queued", "remote_workspace": null, "fallback_reason": null },
        "result_refs": {
            "plan_id": "plan-1",
            "proof_id": null,
            "workspace_mapping_ref": "workspace-1",
            "artifacts": []
        }
    })
}

pub(super) fn lab_runner_handoff_example() -> Value {
    json!({
        "schema": LAB_RUNNER_HANDOFF_ENVELOPE_SCHEMA,
        "status": "succeeded",
        "execution_location": "runner:runner-1",
        "identity": {
            "runner_id": "runner-1",
            "runner_job_id": "job-1",
            "persisted_run_id": "run-1",
            "run_id": "run-1",
            "handoff_id": "runner:runner-1:job:job-1"
        },
        "runner_id": "runner-1",
        "job_id": "job-1",
        "durable_run_id": "run-1",
        "persisted_run_id": "run-1",
        "mirror_run_id": "run-1",
        "remote_cwd": "/home/runner/workspace",
        "path_materialization_plan": {
            "schema": PATH_MATERIALIZATION_PLAN_SCHEMA,
            "entries": [{
                "role": "primary_workspace",
                "owner": "runner_exec.source_snapshot",
                "local_path": "/local/project",
                "remote_path": "/home/runner/workspace",
                "materialization_mode": "snapshot",
                "validation_status": "materialized"
            }]
        },
        "artifact_manifest": {
            "schema": RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
            "manifest_schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
            "path": runner_artifact_manifest_example_path()
        },
        "run_location_index": run_location_index_example(),
        "evidence": {
            "schema": "homeboy/runner-handoff-evidence/v1",
            "status": "succeeded",
            "runner_id": "runner-1",
            "runner_job_id": "job-1",
            "run_id": "run-1",
            "persisted_run_id": "run-1",
            "mirror_run_id": "run-1",
            "remote_cwd": "/home/runner/workspace",
            "artifact_manifest": {
                "schema": RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
                "manifest_schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
                "path": runner_artifact_manifest_example_path()
            },
            "artifact_refs": [{
                "id": "artifact_manifest",
                "name": "runner artifact manifest",
                "path": runner_artifact_manifest_example_path()
            }, {
                "id": "run_location_index",
                "name": "run location index",
                "path": runner_run_location_index_example_path()
            }],
            "next_commands": [{
                "label": "runner_job_logs",
                "command": ["homeboy", "runner", "job", "logs", "runner-1", "job-1", "--follow"]
            }, {
                "label": "run_artifacts",
                "command": ["homeboy", "agent-task", "artifacts", "run-1"]
            }]
        },
        "follow_commands": {
            "job_logs": "homeboy runner job logs runner-1 job-1 --follow",
            "job_cancel": "homeboy runner job cancel runner-1 job-1",
            "status": "homeboy agent-task status run-1",
            "logs": "homeboy agent-task logs run-1",
            "artifacts": "homeboy agent-task artifacts run-1"
        }
    })
}

pub(super) fn run_location_index_example() -> Value {
    json!({
        "schema": RUN_LOCATION_INDEX_SCHEMA,
        "run_id": "run-1",
        "controller_location": "controller:local",
        "runner_id": "runner-1",
        "remote_job_id": "job-1",
        "remote_cwd": "/home/runner/workspace",
        "artifact_manifest_ref": {
            "schema": RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
            "manifest_schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
            "path": runner_artifact_manifest_example_path()
        },
        "liveness_heartbeat_timestamp": "2026-01-01T00:00:00Z",
        "follow_commands": {
            "job_logs": "homeboy runner job logs runner-1 job-1 --follow",
            "job_cancel": "homeboy runner job cancel runner-1 job-1",
            "status": "homeboy agent-task status run-1",
            "logs": "homeboy agent-task logs run-1",
            "artifacts": "homeboy agent-task artifacts run-1"
        }
    })
}

pub(super) fn runner_artifact_manifest_example_path() -> String {
    format!(
        "/home/runner/workspace{RUNNER_ARTIFACT_ROOT_DIR_SUFFIX}/{RUNNER_ARTIFACT_MANIFEST_FILE}"
    )
}

pub(super) fn runner_run_location_index_example_path() -> String {
    format!(
        "/home/runner/workspace{RUNNER_ARTIFACT_ROOT_DIR_SUFFIX}/homeboy-run-location-index.json"
    )
}

pub(super) fn resource_lifecycle_index_example() -> Value {
    json!({
        "schema": RESOURCE_LIFECYCLE_INDEX_SCHEMA,
        "resources": [
            {
                "owner": "homeboy-core",
                "run_id": "run-1",
                "runner_id": "runner-1",
                "path": "/home/runner/workspace-homeboy-artifacts",
                "kind": "artifact_dir",
                "ttl": "P7D",
                "cleanup_policy": ResourceCleanupPolicy::DeleteAfterTtl,
                "evidence_retention": ResourceEvidenceRetention::Manifest,
                "cleanup_intent": "dry_run",
                "status": ResourceLifecycleResourceStatus::Active
            }
        ]
    })
}

pub(super) fn host_mutation_lifecycle_example() -> Value {
    json!({
        "schema": HOST_MUTATION_LIFECYCLE_SCHEMA,
        "owner": "homeboy-core",
        "run_id": "run-1",
        "mutations": [
            {
                "id": "link-current-release",
                "actor": "host-materializer",
                "kind": "symlink",
                "link_path": "/tmp/homeboy/current",
                "target_path": "/tmp/homeboy/releases/run-1",
                "status": HostMutationStatus::Applied,
                "revert": {
                    "strategy": HostMutationRevertStrategy::RemovePath
                }
            },
            {
                "id": "rewrite-package-manifest",
                "actor": "dependency-materializer",
                "kind": "package_manifest_rewrite",
                "manifest_path": "package.json",
                "package_manager": "npm",
                "changes": [
                    {
                        "package": "example-package",
                        "before": "^1.0.0",
                        "after": "^1.1.0"
                    }
                ],
                "status": HostMutationStatus::Applied,
                "revert": {
                    "strategy": HostMutationRevertStrategy::RestorePackageManifest,
                    "backup_path": "package.json.homeboy-backup"
                }
            }
        ]
    })
}
