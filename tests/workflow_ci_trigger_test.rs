use serde_yml::Value;

fn ci_workflow() -> Value {
    let raw = include_str!("../.github/workflows/ci.yml");
    serde_yml::from_str(raw).expect("ci.yml is valid YAML")
}

fn release_workflow() -> Value {
    let raw = include_str!("../.github/workflows/release.yml");
    serde_yml::from_str(raw).expect("release.yml is valid YAML")
}

// `on` is a YAML 1.1 boolean, so serde_yml may key the trigger map as `true`;
// handling both keeps the assertions implementable either way.
fn on_field(workflow: &Value) -> Value {
    workflow
        .get("on")
        .or_else(|| workflow.get(true.to_string().as_str()))
        .expect("ci.yml declares triggers via 'on'")
        .clone()
}

fn pull_request_trigger(workflow: &Value) -> Value {
    on_field(workflow)
        .get("pull_request")
        .expect("ci.yml triggers on pull_request")
        .clone()
}

// Issue #14704: a PR opened against a non-main base must run CI. Any `branches`
// filter on the pull_request trigger excludes stacked PRs, so the trigger must
// have no branch restriction.
#[test]
fn ci_pull_request_trigger_has_no_branch_filter() {
    let workflow = ci_workflow();
    let trigger = pull_request_trigger(&workflow);
    let trigger = trigger
        .as_mapping()
        .expect("pull_request trigger is a mapping");
    assert!(
        !trigger.contains_key(Value::String("branches".to_string())),
        "pull_request trigger must not filter by base branch"
    );
}

// Issue #14704: retargeting a PR emits `pull_request.edited`, which is not a
// default activity type, so the trigger must list `edited` explicitly.
#[test]
fn ci_pull_request_trigger_includes_edited_activity_type() {
    let workflow = ci_workflow();
    let types = pull_request_trigger(&workflow)
        .get("types")
        .and_then(|t| t.as_sequence().cloned())
        .expect("pull_request trigger declares explicit activity types");
    assert!(
        types.contains(&Value::String("edited".to_string())),
        "pull_request trigger must include 'edited' so base retargets schedule checks"
    );
}

// Release lint/test gates must execute the same reviewed Homeboy Action
// revision as PR CI. A floating tag can silently change the gate contract.
#[test]
fn release_quality_gates_use_the_ci_action_revision() {
    let workflow = release_workflow();
    let yaml = serde_yml::to_string(&workflow).expect("serialize release workflow");
    assert_eq!(
        yaml.matches("Extra-Chill/homeboy-action@f1805406709252e4ab25f983b3c72cdba113f2cf")
            .count(),
        2,
        "release lint and test must use the pinned CI action revision"
    );
}

fn prepare_step(name: &str) -> Value {
    release_workflow()["jobs"]["prepare"]["steps"]
        .as_sequence()
        .expect("prepare declares steps")
        .iter()
        .find(|step| step["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("prepare declares the '{name}' step"))
        .clone()
}

/// Runs the "Mark prepared release" script with its declared release outputs
/// and returns everything the step wrote to `$GITHUB_OUTPUT`.
fn mark_prepared_outputs(released: &str, skipped_reason: &str) -> String {
    let script = prepare_step("Mark prepared release for downstream publish")["run"]
        .as_str()
        .expect("the mark step is a run script")
        .to_string();
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("github_output");
    std::fs::write(&output, "").expect("seed GITHUB_OUTPUT");
    let status = std::process::Command::new("bash")
        .args(["-e", "-c", &script])
        .env("GITHUB_OUTPUT", &output)
        .env("RELEASE_TAG", "v9.9.9")
        .env("RELEASED", released)
        .env("SKIPPED_REASON", skipped_reason)
        .status()
        .expect("run bash");
    assert!(status.success(), "the mark step must not fail the release");
    std::fs::read_to_string(&output).expect("read GITHUB_OUTPUT")
}

// When two main pushes race through `check` with the same predicted version,
// the later `prepare` finds the tag already at HEAD. The earlier run owns that
// tag and publishes it. The later run must not hand it to its own build/host
// chain: rebuilding the tag produces a different `dist-manifest.json`, which
// fails publication with "conflicts with the canonical publication bytes"
// (v0.428.22, run 38965622773).
#[test]
fn prepare_stands_down_when_an_earlier_run_already_tagged_head() {
    let outputs = mark_prepared_outputs("false", "release-already-at-head");
    assert!(
        !outputs.contains("prepared=true"),
        "a run that did not cut the tag must not publish it: {outputs}"
    );
    assert!(outputs.contains("tag-owned-elsewhere=true"), "{outputs}");

    let resolve = prepare_step("Resolve release outputs")["run"]
        .as_str()
        .expect("resolve is a run script")
        .to_string();
    assert!(
        resolve.contains("steps.prepared.outputs['tag-owned-elsewhere']")
            && resolve.contains("prepared=false"),
        "resolve must turn a stand-down into a clean prepared=false handoff"
    );
}

#[test]
fn prepare_hands_a_freshly_cut_tag_to_downstream_publication() {
    let outputs = mark_prepared_outputs("true", "");
    assert!(outputs.contains("prepared=true"), "{outputs}");
}
