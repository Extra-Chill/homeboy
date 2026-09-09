use std::{
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

const CANDIDATE: &str = include_str!("../.github/required-gates-ruleset.json");
static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn run_validator(live: &str) -> std::process::Output {
    let fixture = std::env::temp_dir().join(format!(
        "homeboy-required-gates-ruleset-{}-{}.json",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&fixture, live).expect("write live ruleset fixture");
    let output = Command::new("bash")
        .args([".github/validate-required-gates-ruleset.sh", "--github"])
        .env("REQUIRED_GATES_LIVE_RULESET", &fixture)
        .env(
            "REQUIRED_GATES_HEAD_SHA",
            "0123456789012345678901234567890123456789",
        )
        .output()
        .expect("run ruleset validator");
    let _ = std::fs::remove_file(fixture);
    output
}

fn live_ruleset(contexts: serde_json::Value) -> String {
    serde_json::json!({
        "id": 13680120,
        "name": "main",
        "target": "branch",
        "enforcement": "active",
        "bypass_actors": [],
        "rules": [
            { "type": "deletion" },
            { "type": "non_fast_forward" },
            { "type": "required_status_checks", "parameters": {
                "do_not_enforce_on_create": false,
                "strict_required_status_checks_policy": true,
                "required_status_checks": contexts
            }}
        ]
    })
    .to_string()
}

#[test]
fn candidate_requires_the_terminal_context_and_ci_emits_it() {
    let candidate: serde_json::Value = serde_json::from_str(CANDIDATE).expect("candidate JSON");
    let contexts = &candidate["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .find(|rule| rule["type"] == "required_status_checks")
        .expect("required-status-check rule")["parameters"]["required_status_checks"];
    assert!(contexts
        .as_array()
        .expect("contexts")
        .iter()
        .any(|check| check["context"] == "homeboy / Required Gates Executed"));

    let workflow = include_str!("../.github/workflows/ci.yml");
    assert!(workflow.contains("name: homeboy / Required Gates Executed"));
    assert!(workflow.contains("if: ${{ always() }}"));
    assert!(workflow.contains("CI_GATE_RESULTS: ${{ toJSON(needs) }}"));
}

#[test]
fn live_ruleset_validation_fails_when_status_checks_are_absent_or_diverge() {
    let absent = serde_json::json!({
        "id": 13680120,
        "name": "main",
        "target": "branch",
        "enforcement": "active",
        "bypass_actors": [],
        "rules": [{ "type": "deletion" }, { "type": "non_fast_forward" }]
    })
    .to_string();
    let absent_output = run_validator(&absent);
    assert!(!absent_output.status.success());
    assert!(String::from_utf8_lossy(&absent_output.stdout).contains("outcome=absent"));

    let divergent_output = run_validator(
        live_ruleset(serde_json::json!([
            { "context": "homeboy / Rustfmt", "integration_id": 15368 }
        ]))
        .as_str(),
    );
    assert!(!divergent_output.status.success());
    assert!(String::from_utf8_lossy(&divergent_output.stdout).contains("outcome=divergent"));
}

#[test]
fn terminal_gate_rejects_cancelled_required_work() {
    let output = Command::new("bash")
        .arg(".github/ci-required-gates-executed.sh")
        .env("REQUIRED_GATES_HEAD_SHA", "0123456789012345678901234567890123456789")
        .env("CI_GATE_RESULTS", r#"{"rustfmt":{"result":"success"},"lint":{"result":"cancelled"},"homeboy":{"result":"success"}}"#)
        .output()
        .expect("run terminal gate");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("lint=cancelled"));
}
