use std::path::PathBuf;
use std::process::{Command, Output};

fn ruleset() -> &'static str {
    include_str!("../.github/required-gates-ruleset.json")
}

fn ci_workflow() -> &'static str {
    include_str!("../.github/workflows/ci.yml")
}

fn declared_contexts() -> Vec<String> {
    let policy: serde_json::Value = serde_json::from_str(ruleset()).expect("valid ruleset JSON");
    policy["rules"]
        .as_array()
        .expect("ruleset rules")
        .iter()
        .find(|rule| rule["type"] == "required_status_checks")
        .expect("required-status-checks rule")["parameters"]["required_status_checks"]
        .as_array()
        .expect("required check list")
        .iter()
        .map(|check| {
            check["context"]
                .as_str()
                .expect("check context")
                .to_string()
        })
        .collect()
}

/// A throwaway directory for live-state fixtures. The validator's live probe is
/// file-substitutable precisely so these tests never need a network or a token.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "homeboy-required-gates-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Self { dir }
    }

    fn write(&self, name: &str, body: &str) -> String {
        let path = self.dir.join(name);
        std::fs::write(&path, body).expect("fixture write");
        path.to_string_lossy().into_owned()
    }

    fn missing(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The shape `GET /repos/{owner}/{repo}/rules/branches/{branch}` returns: a flat
/// array of effective rules, built here from the *declared* contexts so the
/// fixture cannot drift away from the versioned payload.
fn live_rules(contexts: &[String], strict: bool) -> String {
    let checks: Vec<serde_json::Value> = contexts
        .iter()
        .map(|context| serde_json::json!({ "context": context, "integration_id": 15368 }))
        .collect();
    serde_json::to_string_pretty(&serde_json::json!([
        { "type": "deletion" },
        { "type": "non_fast_forward" },
        {
            "type": "required_status_checks",
            "parameters": {
                "strict_required_status_checks_policy": strict,
                "do_not_enforce_on_create": false,
                "required_status_checks": checks,
            }
        }
    ]))
    .expect("live rules fixture")
}

fn no_required_checks() -> String {
    serde_json::to_string_pretty(&serde_json::json!([
        { "type": "deletion" },
        { "type": "non_fast_forward" }
    ]))
    .expect("live rules fixture")
}

fn ruleset_detail(bypass_actors: serde_json::Value, current_user_can_bypass: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "id": 13680120,
        "name": "main",
        "enforcement": "active",
        "bypass_actors": bypass_actors,
        "current_user_can_bypass": current_user_can_bypass,
    }))
    .expect("ruleset fixture")
}

fn run_validator(mode: &str, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new("bash");
    command.args([".github/validate-required-gates.sh", mode]);
    // Never inherit a real Actions environment: the validator appends evidence
    // to these files when they are set.
    command.env_remove("GITHUB_OUTPUT");
    command.env_remove("GITHUB_STEP_SUMMARY");
    command.env(
        "REQUIRED_GATES_HEAD_SHA",
        "0000000000000000000000000000000000000000",
    );
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .expect("required-gates validator should run")
}

fn run_execution_gate(env: &[(&str, &str)]) -> Output {
    let mut command = Command::new("bash");
    command.arg(".github/ci-required-gates-executed.sh");
    command.env_remove("GITHUB_STEP_SUMMARY");
    command.env("CI_GATE_RESULTS", r#"{"gates":{"result":"success"}}"#);
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .expect("required-gates execution gate should run")
}

fn execution_jobs(contexts: &[String], exception: Option<(&str, serde_json::Value)>) -> String {
    serde_json::to_string(
        &contexts
            .iter()
            .map(|context| {
                serde_json::json!({
                    "name": context,
                    "conclusion": exception
                        .as_ref()
                        .filter(|(candidate, _)| *candidate == context)
                        .map(|(_, conclusion)| conclusion.clone())
                        .unwrap_or_else(|| serde_json::json!("success")),
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("execution jobs fixture")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn required_gate_policy_is_complete_and_emitted_by_every_pr_ci_run() {
    let contexts = declared_contexts();
    let policy: serde_json::Value = serde_json::from_str(ruleset()).expect("valid ruleset JSON");

    assert_eq!(
        contexts,
        [
            "homeboy / Required Gates Declaration",
            "homeboy / Workspace Tests Compile",
            "homeboy / Windows Compile",
            "homeboy / Rustfmt",
            "homeboy / Audit",
            "homeboy / Lint",
            "homeboy / Test",
            "homeboy / Required Gates Executed",
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
    assert_eq!(
        policy["reconcile_preflight"]["required_context"],
        "homeboy / Test",
        "the reconcile preflight must derive its canonical API check name and app id from a declared required context"
    );

    for context in &contexts {
        let matrix_title = context.trim_start_matches("homeboy / ");
        let reusable_test = context == "homeboy / Test"
            && ci_workflow().contains("uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@")
            && ci_workflow().contains("commands: review test");
        // Anchored to a whole YAML key. A bare `contains("title: Test")` also
        // matches `comment-section-title: Test`, which made this assertion — and
        // the shipped validator's matching branch — vacuous for `homeboy / Test`.
        let declares_matrix_title = ci_workflow().contains("name: homeboy / ${{ matrix.title }}")
            && ci_workflow()
                .lines()
                .any(|line| line.trim() == format!("title: {matrix_title}"));
        assert!(
            ci_workflow().contains(&format!("name: {context}"))
                || declares_matrix_title
                || reusable_test,
            "required context {context:?} is not emitted by the always-run CI workflow"
        );
    }

    // The reusable-workflow branch is the ONLY thing that can legitimately
    // satisfy `homeboy / Test`: there is no literal `name: homeboy / Test`, and
    // no matrix entry titled Test. Pin that, so a future edit cannot leave the
    // context green through an unrelated substring again.
    assert!(
        !ci_workflow().contains("name: homeboy / Test"),
        "if a literal Test job appears, the reusable-workflow branch below is no longer load-bearing and this test must be revisited"
    );
    assert!(
        !ci_workflow()
            .lines()
            .any(|line| line.trim() == "title: Test"),
        "no matrix entry is titled Test, so the matrix branch must not be what keeps `homeboy / Test` declared"
    );
    assert!(
        !ci_workflow().contains("paths:"),
        "a required CI check cannot be path-filtered because unrelated PRs would wait forever"
    );

    let test_gate = ci_workflow()
        .split("  homeboy:\n")
        .nth(1)
        .expect("reusable Test gate");
    assert!(test_gate.contains("      scope: auto"));
    assert!(test_gate.contains("      differential-gating: 'true'"));
    assert!(test_gate.contains("      baseline-commands: review test"));
    assert!(test_gate
        .contains("      test-shards: ${{ needs.ci-capacity-admission.outputs.test-shards }}"));
    assert!(test_gate.contains("      execution-timeout-seconds: '2100'"));
    assert!(test_gate.contains("      test-timeout-seconds: '1800'"));
}

#[test]
fn shipped_validator_accepts_the_versioned_policy() {
    let output = run_validator("--local", &[]);
    assert!(output.status.success(), "{}", stderr_of(&output));
}

/// The check's own name is half of the claim it makes. "Required Gates Policy"
/// read as "GitHub requires these gates"; the job only ever proved the
/// declaration, which is what its name now says (#11084).
#[test]
fn ci_job_claims_only_the_declaration_and_reports_live_enforcement() {
    assert!(
        ci_workflow().contains("name: homeboy / Required Gates Declaration"),
        "the CI job must be named for the claim its green tick actually proves"
    );
    assert!(
        !ci_workflow().contains("name: homeboy / Required Gates Policy"),
        "'Required Gates Policy' asserts live enforcement this job does not verify"
    );
    assert!(
        ci_workflow().contains("bash .github/validate-action-pin.sh"),
        "the declaration job must verify reusable-workflow pins resolve to commits"
    );
    assert!(
        ci_workflow().contains("bash .github/validate-required-gates.sh --report"),
        "CI must run the reporting mode so live enforcement is surfaced, not assumed"
    );
    assert!(
        !ci_workflow().contains("bash .github/validate-required-gates.sh --local"),
        "--local silently omits the live enforcement outcome from the run"
    );
}

#[test]
fn report_mode_reports_unenforced_when_the_live_ruleset_requires_no_checks() {
    let scratch = Scratch::new("unenforced");
    let rules = scratch.write("rules.json", &no_required_checks());
    let output = run_validator(
        "--report",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            (
                "REQUIRED_GATES_LIVE_RULESET",
                scratch.missing("absent.json").as_str(),
            ),
        ],
    );
    let stdout = stdout_of(&output);

    assert!(
        output.status.success(),
        "reporting must not block a pull request: {}",
        stderr_of(&output)
    );
    assert!(
        stdout.contains("required-gates-live-status=unenforced"),
        "{stdout}"
    );
    assert!(stdout.contains("outcome=unenforced"), "{stdout}");
    assert!(
        stdout.contains("::warning::required-gates:"),
        "an unenforced ruleset must be loud, not a silent green tick: {stdout}"
    );
    assert!(
        stdout.contains("requires NO status checks"),
        "the annotation must state plainly that nothing is enforced: {stdout}"
    );
    assert!(
        !stdout.contains("outcome=enforced"),
        "an unenforced ruleset must never read as enforced: {stdout}"
    );
}

/// Degrade honestly: no token, no `gh`, or an API error must report `unverified`
/// and never `enforced`.
#[test]
fn report_mode_reports_unverified_when_live_state_cannot_be_read() {
    let scratch = Scratch::new("unverified");
    let output = run_validator(
        "--report",
        &[(
            "REQUIRED_GATES_LIVE_RULES",
            scratch.missing("absent.json").as_str(),
        )],
    );
    let stdout = stdout_of(&output);

    assert!(
        output.status.success(),
        "an unreadable live state must not block a pull request: {}",
        stderr_of(&output)
    );
    assert!(
        stdout.contains("required-gates-live-status=unverified"),
        "{stdout}"
    );
    assert!(
        stdout.contains("could NOT be verified"),
        "unverified must be stated, not implied: {stdout}"
    );
    assert!(
        !stdout.contains("outcome=enforced") && !stdout.contains("outcome=bypassable"),
        "an unreadable live state must never be reported as enforcement: {stdout}"
    );
}

#[test]
fn report_mode_reports_enforced_only_when_live_rules_match_the_declaration() {
    let scratch = Scratch::new("enforced");
    let rules = scratch.write("rules.json", &live_rules(&declared_contexts(), true));
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(serde_json::json!([]), "never"),
    );
    let output = run_validator(
        "--report",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
        ],
    );
    let stdout = stdout_of(&output);

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        stdout.contains("required-gates-live-status=enforced"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("::warning::required-gates:"),
        "a genuinely enforced ruleset has nothing to warn about: {stdout}"
    );
}

#[test]
fn report_mode_reports_divergent_when_a_declared_context_is_not_required() {
    let scratch = Scratch::new("divergent");
    let mut contexts = declared_contexts();
    contexts.pop();
    let rules = scratch.write("rules.json", &live_rules(&contexts, true));
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(serde_json::json!([]), "never"),
    );
    let output = run_validator(
        "--report",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
        ],
    );
    let stdout = stdout_of(&output);

    assert!(
        output.status.success(),
        "reporting divergence must not block a pull request: {}",
        stderr_of(&output)
    );
    assert!(
        stdout.contains("required-gates-live-status=divergent"),
        "{stdout}"
    );
    assert!(
        stdout.contains("::warning::required-gates:") && stdout.contains("DISAGREE"),
        "{stdout}"
    );
}

/// A ruleset the merging actor can step over enforces nothing for that actor,
/// so it cannot report the same word as one nobody can bypass.
#[test]
fn report_mode_reports_bypassable_when_actors_can_bypass_the_ruleset() {
    let scratch = Scratch::new("bypassable");
    let rules = scratch.write("rules.json", &live_rules(&declared_contexts(), true));
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(
            serde_json::json!([
                { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }
            ]),
            "always",
        ),
    );
    let output = run_validator(
        "--report",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
        ],
    );
    let stdout = stdout_of(&output);

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        stdout.contains("required-gates-live-status=bypassable"),
        "{stdout}"
    );
    assert!(
        stdout.contains("::warning::required-gates:")
            && stdout.contains("current_user_can_bypass=always"),
        "the merging actor's bypass ability must be surfaced: {stdout}"
    );
}

/// The declaration half stays fail-closed in reporting mode: a renamed or
/// missing job is repository content a pull request can and does break.
#[test]
fn report_mode_still_fails_closed_on_declaration_drift() {
    let scratch = Scratch::new("drift");
    let drifted = serde_json::json!({
        "rules": [{
            "type": "required_status_checks",
            "parameters": {
                "strict_required_status_checks_policy": true,
                "required_status_checks": [{ "context": "homeboy / Nonexistent Gate" }],
            }
        }]
    });
    let config = scratch.write(
        "config.json",
        &serde_json::to_string_pretty(&drifted).expect("config fixture"),
    );
    let rules = scratch.write("rules.json", &no_required_checks());
    let output = run_validator(
        "--report",
        &[
            ("REQUIRED_GATES_CONFIG", config.as_str()),
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
        ],
    );

    assert!(
        !output.status.success(),
        "a context no CI job emits must still fail the declaration check"
    );
    assert!(
        stderr_of(&output).contains("is not emitted by"),
        "{}",
        stderr_of(&output)
    );
    assert!(
        !stdout_of(&output).contains("required-gates-live-status="),
        "a broken declaration must not be dressed up with an enforcement verdict"
    );
}

/// Acceptance criterion: a workflow declaring `homeboy / Test` against a ruleset
/// that requires no checks must fail the administrator verification path.
#[test]
fn github_mode_fails_closed_when_the_live_ruleset_requires_no_checks() {
    let scratch = Scratch::new("github-unenforced");
    let rules = scratch.write("rules.json", &no_required_checks());
    let output = run_validator("--github", &[("REQUIRED_GATES_LIVE_RULES", rules.as_str())]);

    assert!(
        !output.status.success(),
        "--github must fail closed when GitHub requires nothing: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("enforcement is unenforced"),
        "{}",
        stderr_of(&output)
    );
}

/// Acceptance criterion: matching workflow and ruleset contexts must pass.
#[test]
fn github_mode_passes_when_live_enforcement_matches_the_declaration() {
    let scratch = Scratch::new("github-enforced");
    let rules = scratch.write("rules.json", &live_rules(&declared_contexts(), true));
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(serde_json::json!([]), "never"),
    );
    let output = run_validator(
        "--github",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
        ],
    );

    assert!(
        output.status.success(),
        "{}\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("required-gates-live-status=enforced"),
        "{}",
        stdout_of(&output)
    );
}

#[test]
fn github_mode_rejects_rulesets_that_omit_the_terminal_execution_verdict() {
    let scratch = Scratch::new("missing-terminal-verdict");
    let contexts: Vec<String> = declared_contexts()
        .into_iter()
        .filter(|context| context != "homeboy / Required Gates Executed")
        .collect();
    let rules = scratch.write("rules.json", &live_rules(&contexts, true));
    let output = run_validator("--github", &[("REQUIRED_GATES_LIVE_RULES", rules.as_str())]);

    assert!(
        !output.status.success(),
        "a ruleset without the terminal execution verdict permits cancelled or skipped gates to merge: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("enforcement is divergent"),
        "{}",
        stderr_of(&output)
    );
}

/// GitHub only allows a merge once a required context succeeds. These fixtures
/// exercise the terminal context's fail-closed contract for every non-success
/// check state that could otherwise leave a candidate unverified.
#[test]
fn terminal_execution_verdict_rejects_pending_skipped_cancelled_failed_and_absent_contexts() {
    let contexts = declared_contexts();
    let target = "homeboy / Test";

    for (state, conclusion, expected_outcome) in [
        ("pending", serde_json::Value::Null, "failed"),
        ("skipped", serde_json::json!("skipped"), "failed"),
        ("cancelled", serde_json::json!("cancelled"), "failed"),
        ("failed", serde_json::json!("failure"), "failed"),
    ] {
        let scratch = Scratch::new(state);
        let jobs = scratch.write(
            "jobs.json",
            &execution_jobs(&contexts, Some((target, conclusion))),
        );
        let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);

        assert!(
            !output.status.success(),
            "a {state} terminal context must leave the PR unmergeable: {}",
            stdout_of(&output)
        );
        assert!(
            stdout_of(&output).contains(&format!(
                "required-gates-executed-status={expected_outcome}"
            )),
            "a {state} terminal context must have a fail-closed verdict: {}",
            stdout_of(&output)
        );
    }

    let absent: Vec<String> = contexts
        .iter()
        .filter(|context| context.as_str() != target)
        .cloned()
        .collect();
    let scratch = Scratch::new("absent-terminal-context");
    let jobs = scratch.write("jobs.json", &execution_jobs(&absent, None));
    let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);
    assert!(
        !output.status.success()
            && stdout_of(&output).contains("required-gates-executed-status=skipped"),
        "an absent terminal context must leave the PR unmergeable: {}",
        stdout_of(&output)
    );
}

/// The terminal job has no conclusion until this script exits. Its own required
/// context is enforced by GitHub after that exit, so observing it as a running
/// job would make every otherwise-green run fail cyclically.
#[test]
fn terminal_execution_verdict_does_not_require_its_own_in_progress_conclusion() {
    let contexts = declared_contexts();
    let scratch = Scratch::new("terminal-self-observation");
    let jobs = scratch.write(
        "jobs.json",
        &execution_jobs(
            &contexts,
            Some(("homeboy / Required Gates Executed", serde_json::Value::Null)),
        ),
    );
    let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);

    assert!(
        output.status.success(),
        "the terminal context is enforced by GitHub after this job exits: {}\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("required-gates-executed-status=executed"),
        "{}",
        stdout_of(&output)
    );
}

#[test]
fn terminal_execution_verdict_accepts_a_skipped_planning_duplicate_after_success() {
    let contexts = declared_contexts();
    let scratch = Scratch::new("skipped-planning-duplicate");
    let mut jobs: Vec<serde_json::Value> =
        serde_json::from_str(&execution_jobs(&contexts, None)).expect("execution jobs");
    jobs.push(serde_json::json!({
        "name": "homeboy / Test",
        "conclusion": "skipped",
    }));
    let jobs = scratch.write(
        "jobs.json",
        &serde_json::to_string(&jobs).expect("duplicate jobs"),
    );

    let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);
    assert!(
        output.status.success(),
        "a successful canonical Test context remains valid when its planning duplicate is skipped: {}\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
}

#[test]
fn github_mode_fails_closed_when_the_live_ruleset_has_bypass_actors() {
    let scratch = Scratch::new("github-bypassable");
    let rules = scratch.write("rules.json", &live_rules(&declared_contexts(), true));
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(
            serde_json::json!([
                { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }
            ]),
            "always",
        ),
    );
    let output = run_validator(
        "--github",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
        ],
    );

    assert!(
        !output.status.success(),
        "a bypassable ruleset is not enforced: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("enforcement is bypassable"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn github_mode_fails_closed_when_live_state_cannot_be_read() {
    let scratch = Scratch::new("github-unverified");
    let output = run_validator(
        "--github",
        &[(
            "REQUIRED_GATES_LIVE_RULES",
            scratch.missing("absent.json").as_str(),
        )],
    );

    assert!(
        !output.status.success(),
        "an unverifiable ruleset is not a verified one"
    );
    assert!(
        stderr_of(&output).contains("enforcement is unverified"),
        "{}",
        stderr_of(&output)
    );
}

/// Strictness is what forces a check to have run against the PR's own head. A
/// ruleset with the right contexts and `strict` off is not the declared policy.
#[test]
fn strict_policy_off_is_reported_as_divergent() {
    let scratch = Scratch::new("nonstrict");
    let rules = scratch.write("rules.json", &live_rules(&declared_contexts(), false));
    let output = run_validator("--report", &[("REQUIRED_GATES_LIVE_RULES", rules.as_str())]);
    let stdout = stdout_of(&output);

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        stdout.contains("required-gates-live-status=divergent") && stdout.contains("strict=false"),
        "{stdout}"
    );
}

/// The issue asks for evidence, not just a verdict: target branch, ruleset id,
/// declared and live contexts, bypass actors, and the exact head SHA.
#[test]
fn enforcement_evidence_records_branch_ruleset_bypass_and_head_sha() {
    let scratch = Scratch::new("evidence");
    let rules = scratch.write("rules.json", &no_required_checks());
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(serde_json::json!([]), "never"),
    );
    let output = run_validator(
        "--report",
        &[
            ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
            ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
            ("GH_TARGET_BRANCH", "main"),
            ("GH_RULESET_ID", "13680120"),
        ],
    );
    let stdout = stdout_of(&output);

    for fragment in [
        "basis=live-branch-rules",
        "repo=Extra-Chill/homeboy",
        "branch=main",
        "ruleset=13680120",
        "head=0000000000000000000000000000000000000000",
        "declared=8",
        "live=0",
        "bypass_actors=0",
        "current_user_can_bypass=never",
        "outcome=unenforced",
    ] {
        assert!(
            stdout.contains(fragment),
            "enforcement evidence is missing {fragment:?}: {stdout}"
        );
    }
}

/// The constraint this change is built around: truthful reporting must never
/// become enforcement. Every non-enforced outcome exits 0 in reporting mode, so
/// no pull request is newly blocked by any of them.
#[test]
fn reporting_never_blocks_a_pull_request() {
    let scratch = Scratch::new("nonblocking");
    let mut short = declared_contexts();
    short.pop();
    let detail = scratch.write(
        "ruleset.json",
        &ruleset_detail(
            serde_json::json!([
                { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }
            ]),
            "always",
        ),
    );

    let cases = [
        (
            "unenforced",
            scratch.write("none.json", &no_required_checks()),
        ),
        (
            "divergent",
            scratch.write("short.json", &live_rules(&short, true)),
        ),
        (
            "bypassable",
            scratch.write("full.json", &live_rules(&declared_contexts(), true)),
        ),
        ("unverified", scratch.missing("absent.json")),
    ];

    for (expected, rules) in cases {
        let output = run_validator(
            "--report",
            &[
                ("REQUIRED_GATES_LIVE_RULES", rules.as_str()),
                ("REQUIRED_GATES_LIVE_RULESET", detail.as_str()),
            ],
        );
        assert!(
            output.status.success(),
            "reporting {expected} exited non-zero, which would newly block PRs: {}",
            stderr_of(&output)
        );
        assert!(
            stdout_of(&output).contains(&format!("required-gates-live-status={expected}")),
            "expected {expected}: {}",
            stdout_of(&output)
        );
    }
}

#[test]
fn unknown_mode_is_refused() {
    let output = run_validator("--sometimes", &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr_of(&output).contains("usage:"),
        "{}",
        stderr_of(&output)
    );
}

/// The pin validator's parser must be exercisable without network: a workflow
/// with no reusable call has nothing to dereference and must not be treated as a
/// failure. The positive and negative dereferencing paths need the GitHub API
/// and are exercised by the `Required Gates Declaration` job itself.
#[test]
fn action_pin_validator_is_a_noop_without_reusable_workflows() {
    let dir = Scratch::new("action-pin-no-pins");
    let workflow = dir.write(
        "ci.yml",
        "jobs:\n  checkout:\n    steps:\n      - uses: actions/checkout@v6\n",
    );
    let output = Command::new("bash")
        .arg(".github/validate-action-pin.sh")
        .env("ACTION_PIN_WORKFLOW", &workflow)
        .output()
        .expect("run validate-action-pin.sh");
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        stdout_of(&output).contains("nothing to verify"),
        "{}",
        stdout_of(&output)
    );
}

#[test]
fn action_pin_validator_refuses_a_missing_workflow() {
    let output = Command::new("bash")
        .arg(".github/validate-action-pin.sh")
        .env("ACTION_PIN_WORKFLOW", "/nonexistent/ci.yml")
        .output()
        .expect("run validate-action-pin.sh");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("not found"),
        "{}",
        stderr_of(&output)
    );
}
