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
fn commands_without_workloads_have_no_lab_route_contract() {
    for args in [
        ["homeboy", "status"].as_slice(),
        // trace observe stays local to the observed target.
        [
            "homeboy",
            "trace",
            "observe",
            "component",
            "--watch-process",
            "homeboy",
        ]
        .as_slice(),
    ] {
        assert!(
            parsed_command(args)
                .lab_route_contract()
                .expect("route contract")
                .is_none(),
            "{args:?}"
        );
    }
}

#[test]
fn runner_resident_agent_task_reads_are_low_noise_polling() {
    for args in [
        ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
        ["homeboy", "runs", "logs", "agent-task-123"].as_slice(),
        ["homeboy", "runs", "artifacts", "agent-task-123"].as_slice(),
        ["homeboy", "runs", "evidence", "agent-task-123"].as_slice(),
        ["homeboy", "runs", "list", "--kind", "agent-task"].as_slice(),
        [
            "homeboy",
            "runs",
            "list",
            "--kind",
            "agent-task",
            "--active",
        ]
        .as_slice(),
        [
            "homeboy",
            "runs",
            "list",
            "--kind",
            "agent-task",
            "--limit",
            "1",
        ]
        .as_slice(),
    ] {
        let command = parsed_command(args);
        let contract = command
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
    let contract = parsed_command(&["homeboy", "agent-task", "run", "agent-task-123"])
        .lab_contract()
        .expect("runner-resident execution contract");
    assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
    assert!(!contract.routing_policy.read_only_polling);
}

#[test]
fn extension_show_is_controller_local_until_live_readiness_is_requested() {
    let metadata = parsed_command(&["homeboy", "extension", "show", "extension-1"]);
    assert!(metadata
        .lab_route_contract()
        .expect("metadata route contract")
        .is_none());
    assert!(crate::commands::utils::resource_policy::hot_command(&metadata).is_none());

    let live_readiness = parsed_command(&[
        "homeboy",
        "extension",
        "show",
        "extension-1",
        "--live-readiness",
    ]);
    let contract = live_readiness
        .lab_contract()
        .expect("live readiness Lab contract");
    assert_eq!(contract.hot_label, "extension show --live-readiness");
    assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
    assert!(crate::commands::utils::resource_policy::hot_command(&live_readiness).is_some());
}

#[test]
fn cook_and_fanout_coordinators_stay_controller_local() {
    use crate::commands::contract_lab_routing::{
        AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON as COOK_REASON,
        AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON as FANOUT_REASON,
    };
    let cook = [
        "homeboy",
        "agent-task",
        "cook",
        "--prompt",
        "make a change",
        "--to-worktree",
        "homeboy@cook-finalization",
        "--verify",
        "cargo test --lib",
    ];
    let cook_no_finalize = [cook.as_slice(), &["--no-finalize"]].concat();
    let fanout = [
        "homeboy",
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "homeboy",
        "--verify",
        "cargo test --lib",
        "https://github.com/Extra-Chill/homeboy/issues/1",
    ];

    for (args, reason) in [
        (cook.as_slice(), COOK_REASON),
        (cook_no_finalize.as_slice(), COOK_REASON),
        (fanout.as_slice(), FANOUT_REASON),
    ] {
        let contract = parsed_command(args)
            .lab_contract()
            .expect("coordinator contract");
        assert_eq!(
            contract.portability,
            LabCommandPortability::LocalOnly(reason),
            "{args:?}"
        );
        assert!(!contract.routing_policy.default_lab_offload, "{args:?}");
    }
}

#[test]
fn rig_source_management_explains_lab_setup_boundary() {
    for args in [
        ["homeboy", "rig", "install", "./rig-package"].as_slice(),
        ["homeboy", "rig", "update", "demo-rig"].as_slice(),
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

#[test]
fn unscoped_provider_discovery_stays_controller_local() {
    // The semantic guarantee of #9763: an unscoped `agent-task providers` is a
    // read of *this* controller's catalog. Controller and runner differ in
    // extensions, runtime defaults, secrets, and provider readiness, so
    // automatically relocating the read would change what the answer means.
    let contract = parsed_command(&["homeboy", "agent-task", "providers"])
        .lab_contract()
        .expect("provider discovery keeps a Lab contract for explicit placement");

    assert!(
        !contract.routing_policy.default_lab_offload,
        "unscoped provider discovery must not offload automatically"
    );
    assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
    assert_eq!(
        contract.workspace_mode_policy,
        LabWorkspaceModePolicy::RunnerResident,
        "a provider catalog read must not materialize a workload workspace"
    );
    assert!(
        contract.is_portable(),
        "an explicit --runner probe must remain available"
    );
}
