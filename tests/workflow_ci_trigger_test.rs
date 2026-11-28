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

fn gate_action_revision(workflow: &Value, job: &str) -> String {
    workflow["jobs"][job]["steps"]
        .as_sequence()
        .unwrap_or_else(|| panic!("{job} declares steps"))
        .iter()
        .filter_map(|step| step["uses"].as_str())
        .find_map(|uses| uses.strip_prefix("Extra-Chill/homeboy-action@"))
        .unwrap_or_else(|| panic!("{job} runs the Homeboy Action"))
        .to_string()
}

// Release lint/test gates must execute one reviewed Homeboy Action revision.
// A floating tag, or the two gates drifting apart, silently changes the gate
// contract between lint and test.
#[test]
fn release_quality_gates_share_one_pinned_action_revision() {
    let workflow = release_workflow();
    let lint = gate_action_revision(&workflow, "gate-lint");
    let test = gate_action_revision(&workflow, "gate-test");
    assert!(
        lint.len() == 40 && lint.chars().all(|c| c.is_ascii_hexdigit()),
        "release lint gate must pin a full commit SHA, got `{lint}`"
    );
    assert_eq!(
        lint, test,
        "release lint and test gates must share one revision"
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

fn run_failed_sha_cache_cleanup(list_exit: &str) -> std::process::Output {
    let workflow = release_workflow();
    let step = workflow["jobs"]["clear-failure"]["steps"]
        .as_sequence()
        .expect("clear-failure declares steps")
        .iter()
        .find(|step| step["name"].as_str() == Some("Clear failed SHA cache"))
        .expect("clear-failure declares a cache cleanup step");
    let prefix = step["env"]["CACHE_KEY_PREFIX"]
        .as_str()
        .expect("cache cleanup declares its key prefix")
        .replace("${{ github.ref_name }}", "main");
    let script = step["run"].as_str().expect("cache cleanup is a run script");
    let mock = r#"
gh() {
  case "$1 $2" in
    'cache list')
      [[ "$*" == "cache list --repo example/project --key release-last-failed-main- --json id --jq .[].id" ]] || return 99
      if [[ "$LIST_EXIT" != 0 ]]; then return "$LIST_EXIT"; fi
      printf '123\n456\n'
      ;;
    'cache delete')
      printf '%s\n' "$*"
      # Simulate the first entry disappearing after the list call.
      [[ "$3" != 123 ]]
      ;;
    *) return 98 ;;
  esac
}
"#;
    let dir = tempfile::tempdir().expect("tempdir without a checkout");
    std::process::Command::new("bash")
        .args(["-e", "-c", &format!("{mock}\n{script}")])
        .current_dir(dir.path())
        .env("GITHUB_REPOSITORY", "example/project")
        .env("CACHE_KEY_PREFIX", prefix)
        .env("LIST_EXIT", list_exit)
        .output()
        .expect("run cache cleanup in bash")
}

#[test]
fn release_cache_cleanup_uses_repository_context_and_tolerates_absent_entries() {
    let output = run_failed_sha_cache_cleanup("0");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).expect("delete calls are UTF-8"),
        "cache delete 123 --repo example/project\ncache delete 456 --repo example/project\n"
    );
}

#[test]
fn release_cache_cleanup_fails_when_listing_fails() {
    let output = run_failed_sha_cache_cleanup("42");
    assert_eq!(output.status.code(), Some(42), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}
