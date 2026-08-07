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

    for context in &contexts {
        let matrix_title = context.trim_start_matches("homeboy / ");
        let reusable_test = context == "homeboy / Test"
            && ci_workflow()
                .contains("uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2")
            && ci_workflow().contains("commands: review test");
        assert!(
            ci_workflow().contains(&format!("name: {context}"))
                || (ci_workflow().contains("name: homeboy / ${{ matrix.title }}")
                    && ci_workflow().contains(&format!("title: {matrix_title}")))
                || reusable_test,
            "required context {context:?} is not emitted by the always-run CI workflow"
        );
    }
    assert!(
        !ci_workflow().contains("paths:"),
        "a required CI check cannot be path-filtered because unrelated PRs would wait forever"
    );

    let test_gate = ci_workflow()
        .split("  homeboy:\n")
        .nth(1)
        .expect("reusable Test gate");
    assert!(test_gate.contains("      scope: auto"));
    assert!(test_gate.contains("      differential-gating: 'false'"));
    assert!(test_gate.contains("      baseline-commands: none"));
    assert!(test_gate.contains("      test-shards: '16'"));
    assert!(test_gate.contains("      execution-timeout-seconds: '1800'"));
    assert!(test_gate.contains("      test-timeout-seconds: '1500'"));
    assert!(
        !test_gate.contains("baseline-commands: review test"),
        "Test differential gating expands changed PRs to an unbounded full-workspace run"
    );
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
        "declared=7",
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
