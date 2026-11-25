use std::fs;

use crate::test_support::with_isolated_home;

use super::test_fixture::{write_trace_extension, write_trace_rig};
use super::test_support::trace_args_for_rig;
use super::*;

fn run_aggregate_with_guardrail(command: &str) -> (TraceCommandOutput, i32) {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let rig_path = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("rigs")
            .join("studio-rig.json");
        let mut rig_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig"))
                .expect("parse rig");
        rig_json["trace_guardrails"] = serde_json::json!([
            { "label": "behavior smoke", "command": command }
        ]);
        fs::write(
            &rig_path,
            serde_json::to_string_pretty(&rig_json).expect("serialize rig"),
        )
        .expect("write rig");

        run_repeat(TraceArgs {
            aggregate: Some("spans".to_string()),
            ..trace_args_for_rig("studio-rig", "studio", "studio-app-create-site")
        })
        .expect("trace aggregate should run")
    })
}

#[test]
fn trace_aggregate_guardrail_verdict_drives_exit_without_losing_timing_artifacts() {
    let (output, exit_code) = run_aggregate_with_guardrail("true");
    assert_eq!(exit_code, 0);
    let TraceCommandOutput::Aggregate(aggregate) = output else {
        panic!("expected aggregate output");
    };
    assert!(aggregate.passed);
    assert_eq!(aggregate.guardrail_failure_count, 0);
    assert_eq!(aggregate.guardrails[0].label, "behavior smoke");
    assert!(aggregate.guardrails[0].passed);

    let (output, exit_code) = run_aggregate_with_guardrail("sh -c 'printf regression >&2; exit 7'");
    assert_eq!(exit_code, 1);
    let TraceCommandOutput::Aggregate(aggregate) = output else {
        panic!("expected aggregate output");
    };
    assert!(!aggregate.passed);
    assert_eq!(aggregate.status, "fail");
    assert_eq!(aggregate.failure_count, 0);
    assert_eq!(aggregate.guardrail_failure_count, 1);
    assert_eq!(aggregate.runs[0].status, "pass");
    assert!(!aggregate.runs[0].artifact_path.is_empty());
    assert_eq!(aggregate.spans[0].median_ms, Some(125));
    assert!(aggregate.guardrails[0]
        .failure
        .as_deref()
        .unwrap_or_default()
        .contains("regression"));
}
