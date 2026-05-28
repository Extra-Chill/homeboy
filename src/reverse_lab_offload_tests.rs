use super::{
    prepare_lab_runner_for_offload_with, LabRunnerPreparation, LabRunnerSelection,
    LabRunnerSelectionSource,
};

fn reverse_runner_session(runner_id: &str) -> homeboy::core::runner::RunnerSession {
    homeboy::core::runner::RunnerSession {
        runner_id: runner_id.to_string(),
        mode: homeboy::core::runner::RunnerTunnelMode::Reverse,
        role: homeboy::core::runner::RunnerSessionRole::Runner,
        server_id: None,
        controller_id: Some("controller".to_string()),
        broker_url: None,
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        homeboy_version: "test".to_string(),
        connected_at: "2026-05-27T00:00:00Z".to_string(),
    }
}

#[test]
fn lab_runner_preparation_falls_back_for_disconnected_default_reverse_runner_without_connecting() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: homeboy::core::runner::RunnerTunnelMode::Reverse,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(homeboy::core::runner::RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: false,
                state: homeboy::core::runner::RunnerSessionState::Recorded,
                session: Some(reverse_runner_session(runner_id)),
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("reverse runner sessions are supplied by the reverse substrate"),
    )
    .expect("prepared");

    assert_eq!(
        prepared,
        LabRunnerPreparation::FallBackLocal {
            reason: "reverse-connected runner `lab` is not currently connected".to_string()
        }
    );
}

#[test]
fn lab_runner_preparation_errors_for_disconnected_explicit_reverse_runner() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: homeboy::core::runner::RunnerTunnelMode::Reverse,
    };

    let err = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(homeboy::core::runner::RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: false,
                state: homeboy::core::runner::RunnerSessionState::Recorded,
                session: Some(reverse_runner_session(runner_id)),
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("reverse runner sessions are supplied by the reverse substrate"),
    )
    .expect_err("explicit reverse runner should error");

    assert!(err.message.contains("requires reverse runner `lab`"));
}
