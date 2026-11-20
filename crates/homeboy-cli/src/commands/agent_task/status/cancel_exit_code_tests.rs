use super::*;

#[test]
fn unconverged_accepted_cancellation_maps_to_timeout() {
    let result = homeboy_control_plane_contract::ControlPlaneCancelResult {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_CANCEL_RESULT_SCHEMA.to_string(),
        disposition: homeboy_control_plane_contract::ControlPlaneCancelDisposition::Requested,
        terminal: false,
        wait_timeout_seconds: 15,
        waited_seconds: 15,
        poll_count: 15,
        observation_error: None,
    };
    let payload = homeboy_control_plane_contract::ControlPlaneActionPayload {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_CANCEL_RESULT_SCHEMA.to_string(),
        data: serde_json::to_value(result).expect("serialize result"),
    };
    assert_eq!(
        cancel_exit_code(
            homeboy_control_plane_contract::ControlPlaneActionOutcome::Succeeded,
            &payload,
        )
        .expect("exit code"),
        TIMEOUT_EXIT_CODE
    );
}
