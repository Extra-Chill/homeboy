use homeboy_control_plane_contract::{
    ControlPlaneActionRequest, ControlPlaneEventPage, ControlPlaneResult, ControlPlaneRun,
    ControlPlaneRunPage,
};
use serde::{de::DeserializeOwned, Serialize};

fn assert_golden<T>(fixture: &str)
where
    T: DeserializeOwned + Serialize,
{
    let expected: serde_json::Value = serde_json::from_str(fixture).expect("fixture JSON");
    let decoded: T = serde_json::from_value(expected.clone()).expect("fixture contract");
    let actual = serde_json::to_value(decoded).expect("serialize contract");
    assert_eq!(actual, expected);
}

#[test]
fn v1_wire_documents_match_their_golden_shapes() {
    assert_golden::<ControlPlaneRunPage>(include_str!("fixtures/run-page-v1.json"));
    assert_golden::<ControlPlaneRunPage>(include_str!("fixtures/run-page-placement-v1.json"));
    assert_golden::<ControlPlaneActionRequest>(include_str!("fixtures/action-request-v1.json"));
    assert_golden::<ControlPlaneEventPage>(include_str!("fixtures/event-page-v1.json"));
    assert_golden::<ControlPlaneResult<ControlPlaneRun>>(include_str!(
        "fixtures/result-error-v1.json"
    ));
}
