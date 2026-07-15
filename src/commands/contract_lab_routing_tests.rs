//! Contract-level Lab placement regression tests.

use clap::Parser;

use crate::cli_surface::{Cli, Commands};

use crate::command_contract::{LabCommandPortability, LabSourcePathMode, LabWorkspaceModePolicy};

fn parsed_command(args: &[&str]) -> Commands {
    Cli::try_parse_from(args)
        .expect("CLI args should parse")
        .command
}

#[test]
fn non_workload_command_has_no_workload_arguments() {
    assert!(parsed_command(&["homeboy", "status"])
        .lab_route_contract()
        .expect("route contract")
        .is_none());
}

#[test]
fn runner_resident_agent_task_reads_are_low_noise_polling() {
    for args in [
        ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "list"].as_slice(),
        ["homeboy", "agent-task", "active"].as_slice(),
        ["homeboy", "agent-task", "latest"].as_slice(),
    ] {
        let contract = parsed_command(args)
            .lab_contract()
            .expect("runner-resident read polling contract");
        assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
        assert_eq!(
            contract.workspace_mode_policy,
            LabWorkspaceModePolicy::RunnerResident
        );
        assert!(contract.routing_policy.read_only_polling);
    }
}

#[test]
fn runner_resident_agent_task_execution_keeps_full_runner_evidence() {
    for args in [
        ["homeboy", "agent-task", "run", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "run-next"].as_slice(),
        ["homeboy", "agent-task", "controller", "resume", "loop-123"].as_slice(),
    ] {
        let contract = parsed_command(args)
            .lab_contract()
            .expect("runner-resident execution contract");
        assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
        assert!(!contract.routing_policy.read_only_polling);
    }
}

#[test]
fn agent_task_cook_coordinator_stays_controller_local() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--prompt",
        "make a change",
        "--to-worktree",
        "homeboy@cook-finalization",
        "--verify",
        "cargo test --lib",
    ]);

    let contract = command.lab_contract().expect("cook contract");

    assert_eq!(
        contract.portability,
        LabCommandPortability::LocalOnly(
            crate::commands::contract_lab_routing::AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON
        )
    );
    assert!(!contract.routing_policy.default_lab_offload);
}

#[test]
fn agent_task_cook_no_finalize_is_still_controller_local() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--prompt",
        "make a change",
        "--to-worktree",
        "homeboy@cook-finalization",
        "--verify",
        "cargo test --lib",
        "--no-finalize",
    ]);

    let contract = command.lab_contract().expect("cook contract");

    assert_eq!(
        contract.portability,
        LabCommandPortability::LocalOnly(
            crate::commands::contract_lab_routing::AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON
        )
    );
}

#[test]
fn fanout_and_child_cook_coordinators_stay_controller_local() {
    let coordinator = parsed_command(&[
        "homeboy",
        "agent-task",
        "fanout",
        "run-plan",
        "--input",
        "fanout.json",
    ]);
    let coordinator_contract = coordinator.lab_contract().expect("fanout contract");
    assert_eq!(
        coordinator_contract.portability,
        LabCommandPortability::LocalOnly(
            crate::commands::contract_lab_routing::AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON
        )
    );

    let child = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--prompt",
        "implement the assigned task",
        "--to-worktree",
        "homeboy@child-cook",
        "--verify",
        "cargo test --lib",
    ]);
    assert_eq!(
        child
            .lab_contract()
            .expect("child cook contract")
            .portability,
        LabCommandPortability::LocalOnly(
            crate::commands::contract_lab_routing::AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON
        )
    );
}

#[test]
fn agent_task_promote_with_runner_only_source_remains_lab_portable() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "promote",
        "/runner/artifacts/aggregate.json",
        "--to-worktree",
        "homeboy@fix-7964",
        "--runner",
        "homeboy-lab",
        "--placement",
        "lab",
        "--dry-run",
    ]);

    let contract = command
        .lab_contract()
        .expect("promote contract should support Lab offload");

    assert_eq!(
        contract.hot_label,
        crate::command_contract::AGENT_TASK_PROMOTE_LAB_LABEL
    );
    assert_eq!(contract.portability, LabCommandPortability::Portable);
    assert_eq!(
        contract.workspace_mode_policy,
        LabWorkspaceModePolicy::GitCheckoutRequired
    );
    assert!(contract.capture_mutation_patch);
    assert!(!command.lab_offload_captures_mutation_patch());
}

#[test]
fn agent_task_promote_apply_captures_remote_target_mutation_for_controller_handoff() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "promote",
        "/runner/artifacts/aggregate.json",
        "--to-worktree",
        "homeboy@fix-7986",
    ]);

    assert!(command.lab_offload_captures_mutation_patch());
    assert_eq!(command.lab_offload_mutation_flag(), Some("--to-worktree"));
}

#[test]
fn rig_source_management_explains_lab_setup_boundary() {
    for args in [
        ["homeboy", "rig", "install", "./rig-package"].as_slice(),
        ["homeboy", "rig", "update", "demo-rig"].as_slice(),
        ["homeboy", "rig", "sync", "demo-rig"].as_slice(),
        ["homeboy", "rig", "sources"].as_slice(),
    ] {
        let command = parsed_command(args);
        let contract = command
            .lab_contract()
            .expect("rig setup/source command should explain Lab boundary");

        assert_eq!(
            contract.hot_label,
            crate::command_contract::RIG_SOURCE_MANAGEMENT_LAB_LABEL
        );
        assert_eq!(
            contract.portability,
            LabCommandPortability::LocalOnly(
                crate::command_contract::RIG_SOURCE_MANAGEMENT_LAB_UNSUPPORTED_REASON
            )
        );
    }
}
