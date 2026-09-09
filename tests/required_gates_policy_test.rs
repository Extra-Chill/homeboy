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

fn divergent_live_ruleset(mutate: impl FnOnce(&mut serde_json::Value)) -> String {
    let candidate: serde_json::Value = serde_json::from_str(CANDIDATE).expect("candidate JSON");
    let mut live = candidate;
    live["id"] = serde_json::json!(13680120);
    mutate(&mut live);
    live.to_string()
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
fn terminal_job_checks_out_the_pr_head_before_running_its_script() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    let terminal_job = workflow
        .split("  required-gates-executed:")
        .nth(1)
        .expect("terminal job");
    assert!(
        terminal_job.contains("needs: [rustfmt, lint, homeboy]"),
        "the terminal job must depend only on the gates it evaluates"
    );
    let checkout = terminal_job
        .find("- uses: actions/checkout@v6")
        .expect("terminal job checkout");
    let script = terminal_job
        .find("bash .github/ci-required-gates-executed.sh")
        .expect("terminal job script");
    assert!(
        checkout < script,
        "the repository script must be checked out first"
    );
}

#[test]
fn live_ruleset_validation_ignores_status_check_order_and_response_metadata() {
    let live = divergent_live_ruleset(|ruleset| {
        ruleset["node_id"] = serde_json::json!("RRS_kwDOExample");
        ruleset["updated_at"] = serde_json::json!("2026-09-09T13:00:00Z");
        let checks = ruleset["rules"][2]["parameters"]["required_status_checks"]
            .as_array_mut()
            .expect("required status checks");
        checks.reverse();
        checks[0]["url"] = serde_json::json!("https://api.github.com/checks/1");
        checks[0]["node_id"] = serde_json::json!("RSC_kwDOExample");
    });

    let output = run_validator(&live);
    assert!(
        output.status.success(),
        "reordered checks and GitHub response metadata must not cause divergence: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("outcome=enforced"));
}

#[test]
fn live_ruleset_validation_fails_when_status_checks_are_absent_or_declared_contract_diverges() {
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

    for (name, live) in [
        (
            "target-branch",
            divergent_live_ruleset(|ruleset| {
                ruleset["conditions"]["ref_name"]["include"] =
                    serde_json::json!(["refs/heads/release"]);
            }),
        ),
        (
            "missing-required-rule",
            divergent_live_ruleset(|ruleset| {
                ruleset["rules"][1] = serde_json::json!({ "type": "creation" });
            }),
        ),
        (
            "nonstrict",
            divergent_live_ruleset(|ruleset| {
                ruleset["rules"][2]["parameters"]["strict_required_status_checks_policy"] =
                    serde_json::json!(false);
            }),
        ),
        (
            "bypass-actor",
            divergent_live_ruleset(|ruleset| {
                ruleset["bypass_actors"] = serde_json::json!([{ "actor_id": 1, "actor_type": "RepositoryRole", "bypass_mode": "always" }]);
            }),
        ),
        (
            "create-behavior",
            divergent_live_ruleset(|ruleset| {
                ruleset["rules"][2]["parameters"]["do_not_enforce_on_create"] =
                    serde_json::json!(true);
            }),
        ),
    ] {
        let output = run_validator(&live);
        assert!(
            !output.status.success(),
            "{name} divergence must fail the live audit"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("outcome=divergent"),
            "{name} divergence must be reported"
        );
    }
}

#[test]
fn terminal_gate_rejects_cancelled_pending_and_absent_work_for_the_pr_head() {
    const HEAD: &str = "0123456789012345678901234567890123456789";
    for (name, results, expected) in [
        (
            "cancelled",
            r#"{"rustfmt":{"result":"success"},"lint":{"result":"cancelled"},"homeboy":{"result":"success"}}"#,
            "lint=cancelled",
        ),
        (
            "pending",
            r#"{"rustfmt":{"result":"success"},"lint":{"result":"success"},"homeboy":{"result":"pending"}}"#,
            "homeboy=pending",
        ),
        (
            "absent",
            r#"{"rustfmt":{"result":"success"},"lint":{"result":"success"}}"#,
            "missing=[homeboy]",
        ),
    ] {
        let output = Command::new("bash")
            .arg(".github/ci-required-gates-executed.sh")
            .env("REQUIRED_GATES_HEAD_SHA", HEAD)
            .env("CI_GATE_RESULTS", results)
            .output()
            .expect("run terminal gate");
        assert!(
            !output.status.success(),
            "{name} work must fail the PR head"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(HEAD),
            "{name} result must identify the PR head"
        );
        assert!(
            stdout.contains(expected),
            "{name} result must identify the gate state"
        );
    }
}
