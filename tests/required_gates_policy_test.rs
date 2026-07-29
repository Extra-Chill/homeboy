use std::process::Command;

fn ruleset() -> &'static str {
    include_str!("../.github/required-gates-ruleset.json")
}

fn ci_workflow() -> &'static str {
    include_str!("../.github/workflows/ci.yml")
}

#[test]
fn required_gate_policy_is_complete_and_emitted_by_every_pr_ci_run() {
    let policy: serde_json::Value = serde_json::from_str(ruleset()).expect("valid ruleset JSON");
    let checks = policy["rules"]
        .as_array()
        .expect("ruleset rules")
        .iter()
        .find(|rule| rule["type"] == "required_status_checks")
        .expect("required-status-checks rule")["parameters"]["required_status_checks"]
        .as_array()
        .expect("required check list");
    let contexts: Vec<&str> = checks
        .iter()
        .map(|check| check["context"].as_str().expect("check context"))
        .collect();

    assert_eq!(
        contexts,
        [
            "homeboy / Required Gates Policy",
            "homeboy / Workspace Tests Compile",
            "homeboy / Windows Compile",
            "homeboy / Rustfmt",
            "homeboy / CLI Reference Docs",
            "homeboy / Audit",
            "homeboy / Lint",
            "homeboy / Test",
        ],
        "the versioned policy must enumerate every main-merge gate"
    );
    assert_eq!(
        contexts.len(),
        contexts
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "a duplicate context would make the policy ambiguous"
    );
    assert!(policy["rules"].as_array().unwrap().iter().any(|rule| {
        rule["type"] == "required_status_checks"
            && rule["parameters"]["strict_required_status_checks_policy"] == true
    }));

    for context in contexts {
        let matrix_title = context.trim_start_matches("homeboy / ");
        assert!(
            ci_workflow().contains(&format!("name: {context}"))
                || (ci_workflow().contains("name: homeboy / ${{ matrix.title }}")
                    && ci_workflow().contains(&format!("title: {matrix_title}"))),
            "required context {context:?} is not emitted by the always-run CI workflow"
        );
    }
    assert!(
        !ci_workflow().contains("paths:"),
        "a required CI check cannot be path-filtered because unrelated PRs would wait forever"
    );
}

#[test]
fn shipped_validator_accepts_the_versioned_policy() {
    let output = Command::new("bash")
        .args([".github/validate-required-gates.sh", "--local"])
        .output()
        .expect("required-gates validator should run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
