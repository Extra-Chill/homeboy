use super::super::*;
use super::*;

#[test]
fn init_and_status_round_trip_controller_record() {
    with_isolated_home(|_| {
        let record = init(ControllerInitRequest {
            loop_id: "loop-service-init".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        assert_eq!(record.loop_id, "loop-service-init");
        assert_eq!(record.phase, "repair");

        let loaded = status("loop-service-init").expect("controller loaded");
        assert_eq!(loaded, record);
    });
}

#[test]
fn list_returns_existing_controllers() {
    with_isolated_home(|_| {
        init(ControllerInitRequest {
            loop_id: "loop-service-list-a".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller a initialized");
        init(ControllerInitRequest {
            loop_id: "loop-service-list-b".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller b initialized");

        let report = list().expect("controllers listed");
        assert_eq!(report.schema, LIST_RESULT_SCHEMA);
        assert_eq!(report.controllers.len(), 2);
    });
}
