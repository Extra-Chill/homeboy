use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static SCRATCH_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// The one declaration (#13125). Every other artifact in this test is either
/// generated from this file or validated against it; nothing here restates the
/// gate list. A test that hardcodes the thing it pins cannot catch the drift it
/// was written to prevent, which is how `tests/fixtures/` grew a second copy.
fn manifest() -> &'static serde_json::Value {
    static MANIFEST: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!("../.github/required-gates-manifest.json"))
            .expect("valid required-gates manifest JSON")
    })
}

fn gates() -> &'static [serde_json::Value] {
    manifest()["gates"].as_array().expect("manifest gates")
}

/// The generated live-ruleset payload. It is derived from the manifest, so it
/// doubles as the shipped execution policy the validator and terminal gate read.
const RULESET_PATH: &str = ".github/required-gates-ruleset.json";

fn ruleset() -> &'static str {
    include_str!("../.github/required-gates-ruleset.json")
}

fn versioned_ruleset() -> &'static str {
    ruleset()
}

fn ci_workflow() -> &'static str {
    include_str!("../.github/workflows/ci.yml")
}

/// Resolve one gate's context by its stable manifest id. Tests that need a
/// specific gate as a sample name it by id, so renaming a context in the
/// manifest cannot leave a stale literal asserting against the old name.
fn context_of(id: &str) -> String {
    gates()
        .iter()
        .find(|gate| gate["id"] == id)
        .unwrap_or_else(|| panic!("manifest declares no gate with id {id:?}"))["context"]
        .as_str()
        .expect("gate context")
        .to_string()
}

fn declared_contexts() -> Vec<String> {
    gates()
        .iter()
        .filter(|gate| gate["required_status_check"] == true)
        .map(|gate| {
            gate["context"]
                .as_str()
                .expect("manifest gate context")
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
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "homeboy-required-gates-{}-{sequence}-{name}",
            std::process::id(),
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
    command.env("REQUIRED_GATES_CONFIG", RULESET_PATH);
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .expect("required-gates validator should run")
}

/// Every non-terminal gate's declared producer JOB ID.
///
/// This is the key GitHub types into the `needs` context, and since #13125 it is
/// what the terminal gate joins on — matrix legs and reusable-workflow calls
/// arrive pre-aggregated, so nothing in `needs` matches a display context.
fn producer_jobs() -> Vec<String> {
    let mut jobs: Vec<String> = gates()
        .iter()
        .filter(|gate| gate["required_status_check"] == true && gate["terminal"] != true)
        .map(|gate| {
            gate["producer"]["job"]
                .as_str()
                .expect("manifest producer job")
                .to_string()
        })
        .collect();
    jobs.sort();
    jobs.dedup();
    jobs
}

/// `toJSON(needs)` as the terminal gate reads it.
///
/// Derived from the manifest for the same reason everything else here is: this
/// fixture used to be the literal `{"gates":{"result":"success"}}`, which
/// predated the producer-job keying above. `gates` matches no producer job, so
/// every declared gate resolved to `missing-from-needs`, the run short-circuited
/// to `outcome=skipped`, and six tests below spent that time asserting against
/// the short-circuit instead of the contract they are named for (#13561). A
/// fixture that restates the thing it pins cannot catch the drift it exists to
/// prevent.
fn typed_needs(exception: Option<(&str, &str)>) -> String {
    serde_json::to_string(
        &producer_jobs()
            .into_iter()
            .map(|job| {
                let result = exception
                    .filter(|(candidate, _)| *candidate == job)
                    .map(|(_, result)| result)
                    .unwrap_or("success");
                (job, serde_json::json!({ "result": result }))
            })
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    )
    .expect("typed needs fixture")
}

fn run_execution_gate(env: &[(&str, &str)]) -> Output {
    let mut command = Command::new("bash");
    command.arg(".github/ci-required-gates-executed.sh");
    command.env_remove("GITHUB_STEP_SUMMARY");
    command.env("CI_GATE_RESULTS", typed_needs(None));
    command.env(
        "REQUIRED_GATES_HEAD_SHA",
        "0000000000000000000000000000000000000000",
    );
    command.env("REQUIRED_GATES_CONFIG", RULESET_PATH);
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .expect("required-gates execution gate should run")
}

/// A mutated copy of the declaration, written to scratch.
///
/// `REQUIRED_GATES_CONFIG` names the derived ruleset payload, not the
/// declaration. Since #13125 both the validator and the terminal gate read
/// `REQUIRED_GATES_MANIFEST`, so a test that mutates the payload mutates
/// something neither script consults and asserts against the real eight-gate
/// policy without noticing (#13561).
fn scratch_manifest(scratch: &Scratch, mutate: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut mutated = manifest().clone();
    mutate(&mut mutated);
    scratch.write(
        "manifest.json",
        &serde_json::to_string_pretty(&mutated).expect("mutated manifest"),
    )
}

/// A mutated declaration plus the artifacts derived from it, in scratch.
///
/// The validator's first act is to reject a manifest that disagrees with its
/// generated artifacts, so a mutated declaration reaches no other check until
/// its own ruleset payload has been regenerated from it. Returns the env
/// overrides that point every consumer at the scratch set; the documentation
/// artifact is switched off, which the generator supports explicitly.
fn scratch_declaration(
    scratch: &Scratch,
    mutate: impl FnOnce(&mut serde_json::Value),
) -> Vec<(String, String)> {
    let declaration = scratch_manifest(scratch, mutate);
    let ruleset = scratch.missing("required-gates-ruleset.json");

    let generated = Command::new("bash")
        .arg(".github/generate-required-gates-artifacts.sh")
        .env("REQUIRED_GATES_MANIFEST", &declaration)
        .env("REQUIRED_GATES_RULESET_OUTPUT", &ruleset)
        .env("REQUIRED_GATES_DOCS_OUTPUT", "")
        .output()
        .expect("required-gates generator should run");
    assert!(
        generated.status.success(),
        "the mutated declaration must still be derivable, or the test is measuring the \
         generator's refusal instead of the property it names: {}",
        stderr_of(&generated)
    );

    vec![
        ("REQUIRED_GATES_MANIFEST".to_string(), declaration),
        ("REQUIRED_GATES_RULESET_OUTPUT".to_string(), ruleset),
        ("REQUIRED_GATES_DOCS_OUTPUT".to_string(), String::new()),
    ]
}

fn execution_jobs(contexts: &[String], exception: Option<(&str, serde_json::Value)>) -> String {
    serde_json::to_string(
        &contexts
            .iter()
            .map(|context| {
                serde_json::json!({
                    "name": context,
                    "head_sha": "0000000000000000000000000000000000000000",
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
fn required_gate_execution_contract_is_complete_and_emitted_by_every_pr_ci_run() {
    let contexts = declared_contexts();
    let policy: serde_json::Value = serde_json::from_str(ruleset()).expect("valid ruleset JSON");

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
    // Each gate declares HOW it reaches GitHub, so the workflow is validated
    // against the manifest instead of the test guessing per-context. A gate that
    // changes emission mode without changing the workflow fails here.
    for gate in gates()
        .iter()
        .filter(|gate| gate["required_status_check"] == true)
    {
        let context = gate["context"].as_str().expect("gate context");
        let producer = &gate["producer"];
        let emission = producer["emission"].as_str().expect("gate emission");

        let emitted = match emission {
            "job_name" => ci_workflow().contains(&format!("name: {context}")),
            // Anchored to a whole YAML key. A bare `contains("title: Audit")`
            // also matches `comment-section-title: Audit`, which made this
            // assertion vacuous for any matrix leg.
            "matrix_title" => {
                let title = producer["matrix_title"].as_str().expect("matrix title");
                ci_workflow().contains("name: homeboy / ${{ matrix.title }}")
                    && ci_workflow()
                        .lines()
                        .any(|line| line.trim() == format!("title: {title}"))
            }
            "reusable_workflow_call" => {
                let job = producer["job"].as_str().expect("producer job");
                ci_workflow().contains("uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@")
                    && ci_workflow().contains(&format!("\n  {job}:\n"))
            }
            other => panic!("gate {context:?} declares unknown emission {other:?}"),
        };

        assert!(
            emitted,
            "required context {context:?} declares emission {emission:?} but the always-run CI workflow does not emit it that way"
        );

        // A gate must reach GitHub exactly one way. If a literal job name ever
        // appears for a gate declared as a matrix leg or a reusable call, the
        // declared mode has silently stopped being load-bearing.
        if emission != "job_name" {
            assert!(
                !ci_workflow().contains(&format!("name: {context}")),
                "gate {context:?} declares emission {emission:?}, but a literal job named {context:?} also exists — the declared mode is no longer what keeps the context green"
            );
        }
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
    assert!(test_gate.contains("      differential-gating: 'true'"));
    assert!(test_gate.contains("      baseline-commands: review test"));
    assert!(test_gate
        .contains("      test-shards: ${{ needs.ci-capacity-admission.outputs.test-shards }}"));
    assert!(test_gate.contains("      execution-timeout-seconds: '2100'"));
    assert!(test_gate.contains("      test-timeout-seconds: '1800'"));
}

/// This assertion replaces `versioned_ruleset_retains_current_main_no_check_policy`,
/// which asserted the shipped ruleset required NO status checks while the shipped
/// ruleset required eight. That is the exact drift this manifest exists to make
/// impossible: a policy statement and the policy disagreeing, with nothing forcing
/// them to move together. Pin the relationship instead of either side's contents.
#[test]
fn versioned_ruleset_projects_exactly_the_manifest_declaration() {
    let policy: serde_json::Value =
        serde_json::from_str(versioned_ruleset()).expect("valid ruleset JSON");
    let declared = declared_contexts();

    let checks = policy["rules"]
        .as_array()
        .expect("ruleset rules")
        .iter()
        .find(|rule| rule["type"] == "required_status_checks");

    if declared.is_empty() {
        assert!(
            checks.is_none(),
            "a zero-context manifest must project a ruleset with no required_status_checks rule"
        );
        return;
    }

    let parameters = &checks
        .expect("a manifest declaring required gates must project a required_status_checks rule")
        ["parameters"];
    let projected: Vec<String> = parameters["required_status_checks"]
        .as_array()
        .expect("required status checks")
        .iter()
        .map(|check| check["context"].as_str().expect("context").to_string())
        .collect();

    assert_eq!(
        projected, declared,
        "the ruleset payload must project exactly the manifest's declared contexts, in order"
    );
    assert_eq!(
        parameters["strict_required_status_checks_policy"],
        manifest()["status_checks"]["strict_required_status_checks_policy"],
        "strictness is what forces a check to have run against the PR's own head"
    );
}

/// Criterion: the derived artifacts cannot drift from the manifest. Regenerating
/// must be a no-op on a clean tree, so editing a gate anywhere but the manifest —
/// or editing the manifest without regenerating — fails here rather than in
/// production enforcement.
#[test]
fn regenerating_every_derived_artifact_is_a_no_op() {
    let output = Command::new("bash")
        .args([".github/generate-required-gates-artifacts.sh", "--check"])
        .output()
        .expect("required-gates generator should run");

    assert!(
        output.status.success(),
        "a derived artifact drifted from .github/required-gates-manifest.json:\n{}\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
}

/// Criterion: duplicate GitHub contexts are rejected at declaration time, not
/// interpreted after execution. Two gates claiming one context makes both the
/// ruleset payload and the terminal verdict ambiguous.
#[test]
fn a_duplicate_declared_context_is_refused_by_the_generator() {
    let scratch = Scratch::new("duplicate-context");
    let mut mutated = manifest().clone();
    let gates = mutated["gates"].as_array_mut().expect("manifest gates");
    let mut clone = gates[0].clone();
    clone["id"] = serde_json::json!("declaration-duplicate");
    gates.push(clone);
    let path = scratch.write(
        "manifest.json",
        &serde_json::to_string_pretty(&mutated).expect("mutated manifest"),
    );

    let output = Command::new("bash")
        .args([".github/generate-required-gates-artifacts.sh", "--check"])
        .env("REQUIRED_GATES_MANIFEST", &path)
        .env(
            "REQUIRED_GATES_RULESET_OUTPUT",
            scratch.missing("ruleset.json"),
        )
        .env("REQUIRED_GATES_DOCS_OUTPUT", "")
        .output()
        .expect("required-gates generator should run");

    assert!(
        !output.status.success(),
        "two gates claiming one context must be refused before any execution is interpreted"
    );
    assert!(
        stderr_of(&output).contains("duplicate"),
        "the refusal must name the ambiguity: {}",
        stderr_of(&output)
    );
}

/// Criterion: a declaration that gates nothing is refused at declaration time.
///
/// Every derived artifact is a projection of `.gates`, so a manifest that
/// declares no gates — or that has lost the terminal aggregate proving the
/// others executed — derives a ruleset requiring nothing, and `--check` would
/// then report that vacuum as *current*. This is #12833 reached through
/// generation rather than through live drift: the enforceable difference
/// between "the policy is satisfied" and "there is no policy" has to be made
/// here, because downstream every artifact is faithful to whatever it was
/// given.
#[test]
fn a_declaration_that_gates_nothing_is_refused_by_the_generator() {
    for (label, mutate) in [
        (
            "no-gates",
            (|manifest: &mut serde_json::Value| {
                manifest["gates"] = serde_json::json!([]);
            }) as fn(&mut serde_json::Value),
        ),
        ("no-terminal-gate", |manifest: &mut serde_json::Value| {
            for gate in manifest["gates"].as_array_mut().expect("manifest gates") {
                if let Some(object) = gate.as_object_mut() {
                    object.remove("terminal");
                }
            }
        }),
    ] {
        let scratch = Scratch::new(label);
        let mut mutated = manifest().clone();
        mutate(&mut mutated);
        let path = scratch.write(
            "manifest.json",
            &serde_json::to_string_pretty(&mutated).expect("mutated manifest"),
        );

        let output = Command::new("bash")
            .args([".github/generate-required-gates-artifacts.sh", "--check"])
            .env("REQUIRED_GATES_MANIFEST", &path)
            .env(
                "REQUIRED_GATES_RULESET_OUTPUT",
                scratch.missing("ruleset.json"),
            )
            .env("REQUIRED_GATES_DOCS_OUTPUT", "")
            .output()
            .expect("required-gates generator should run");

        assert!(
            !output.status.success(),
            "the `{label}` manifest declares no enforceable gate policy and must be refused \
             before any artifact is derived from it"
        );
    }
}

#[test]
fn shipped_validator_accepts_the_versioned_policy() {
    let output = run_validator(
        "--local",
        &[(
            "REQUIRED_GATES_CONFIG",
            ".github/required-gates-ruleset.json",
        )],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
}

/// The check's own name is half of the claim it makes. "Required Gates Policy"
/// read as "GitHub requires these gates"; the job only ever proved the
/// declaration, which is what its name now says (#11084).
#[test]
fn ci_job_claims_only_the_declaration_and_reports_live_enforcement() {
    assert!(
        ci_workflow().contains(&format!("name: {}", context_of("declaration"))),
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
    // Rename one declared gate's context to something `ci.yml` emits nowhere.
    // The declaration is the manifest, so drifting it means drifting that.
    let declaration = scratch_declaration(&scratch, |manifest| {
        manifest["gates"][0]["context"] = serde_json::json!("homeboy / Nonexistent Gate");
    });
    let rules = scratch.write("rules.json", &no_required_checks());
    let mut env: Vec<(&str, &str)> = declaration
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    env.push(("REQUIRED_GATES_LIVE_RULES", rules.as_str()));
    let output = run_validator("--report", &env);

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
        .filter(|context| *context != context_of("executed"))
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

/// **The reason the six tests below can be trusted at all.**
///
/// Every one of them injects job conclusions and asserts on the verdict. None
/// of that is reachable if the typed-needs join fails first: an unmatched
/// producer job is `missing-from-needs`, which lands in the skipped class and
/// returns `outcome=skipped` before a single conclusion is read. That is
/// exactly what happened for as long as `CI_GATE_RESULTS` was the literal
/// `{"gates":{"result":"success"}}` (#13561).
///
/// So the join is asserted directly, rather than assumed by the tests that
/// depend on it. A guard that only fires when something else is already broken
/// is a guard that reports the symptom; this one reports the cause.
#[test]
fn the_typed_needs_fixture_resolves_every_declared_producer_job() {
    let contexts = declared_contexts();
    let scratch = Scratch::new("typed-needs-join");
    let jobs = scratch.write("jobs.json", &execution_jobs(&contexts, None));
    let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);

    assert!(
        !stdout_of(&output).contains("missing-from-needs"),
        "the typed-needs fixture no longer resolves every producer job, so every test below is \
         asserting against a short-circuit rather than the contract it names: {}",
        stdout_of(&output)
    );
    assert!(
        stdout_of(&output).contains("required-gates-executed-status=executed"),
        "a fully successful run must reach the executed verdict: {}",
        stdout_of(&output)
    );
    assert!(
        !producer_jobs().is_empty(),
        "the fixture is derived from the manifest, so an empty producer set would make the \
         assertions above pass vacuously"
    );
}

/// GitHub only allows a merge once a required context succeeds. These fixtures
/// exercise the terminal context's fail-closed contract for every non-success
/// check state that could otherwise leave a candidate unverified.
#[test]
fn terminal_execution_verdict_rejects_pending_skipped_cancelled_failed_and_absent_contexts() {
    let contexts = declared_contexts();
    let target = context_of("test");
    let target = target.as_str();

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

#[test]
fn terminal_execution_verdict_reports_reporting_only_policy_without_probing_jobs() {
    let scratch = Scratch::new("reporting-only-execution");
    // A reporting-only stance is a declaration that requires no status check,
    // and the terminal gate reads that from the manifest's own
    // `zero_context_policy` -- not from the derived ruleset payload.
    let declaration = scratch_manifest(&scratch, |manifest| {
        for gate in manifest["gates"].as_array_mut().expect("manifest gates") {
            gate["required_status_check"] = serde_json::json!(false);
        }
    });
    let output = run_execution_gate(&[
        ("REQUIRED_GATES_MANIFEST", declaration.as_str()),
        // A reporting-only policy has no execution evidence to verify.
        (
            "REQUIRED_GATES_EXECUTED_JOBS",
            scratch.missing("jobs.json").as_str(),
        ),
    ]);

    assert!(
        output.status.success(),
        "a zero-context policy is reporting-only: {}\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("required-gates-executed-status=not-required")
            && stdout_of(&output).contains("basis=reporting-only-policy"),
        "{}",
        stdout_of(&output)
    );
}

#[test]
fn terminal_execution_verdict_rejects_a_malformed_ruleset() {
    let scratch = Scratch::new("malformed-execution-policy");
    let policy = scratch.write("ruleset.json", "not JSON");
    let output = run_execution_gate(&[("REQUIRED_GATES_CONFIG", policy.as_str())]);

    assert!(
        !output.status.success(),
        "a malformed policy must not be reported as a successful execution"
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
            Some((context_of("executed").as_str(), serde_json::Value::Null)),
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
        "name": context_of("test"),
        "head_sha": "0000000000000000000000000000000000000000",
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
    assert!(
        stdout_of(&output).contains("\"raw_conclusions\":[\"success\",\"skipped\"]")
            && stdout_of(&output).contains("\"selected_conclusion\":\"success\""),
        "duplicate diagnostics must retain both results and the selected success: {}",
        stdout_of(&output)
    );
    assert!(
        stdout_of(&output).contains("declared=8 executed=8"),
        "the terminal gate counts itself after observing the other seven: {}",
        stdout_of(&output)
    );
}

#[test]
fn terminal_execution_verdict_keeps_duplicate_failures_and_cancellations_fail_closed() {
    let contexts = declared_contexts();

    for (name, conclusion) in [("failure", "failure"), ("cancelled", "cancelled")] {
        let scratch = Scratch::new(name);
        let mut jobs: Vec<serde_json::Value> =
            serde_json::from_str(&execution_jobs(&contexts, None)).expect("execution jobs");
        jobs.push(serde_json::json!({
            "name": context_of("test"),
            "head_sha": "0000000000000000000000000000000000000000",
            "conclusion": conclusion,
        }));
        let jobs = scratch.write(
            "jobs.json",
            &serde_json::to_string(&jobs).expect("duplicate jobs"),
        );
        let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);

        assert!(
            !output.status.success()
                && stdout_of(&output).contains("required-gates-executed-status=failed"),
            "a {name} duplicate must not be masked by success: {}",
            stdout_of(&output)
        );
        assert!(
            stdout_of(&output).contains(&format!("\"selected_conclusion\":\"{conclusion}\"")),
            "the selected fail-closed result must be diagnosable: {}",
            stdout_of(&output)
        );
    }
}

#[test]
fn terminal_execution_verdict_rejects_skipped_only_and_ignores_other_head_duplicates() {
    let contexts = declared_contexts();
    let scratch = Scratch::new("skipped-only-other-head");
    let mut jobs: Vec<serde_json::Value> = serde_json::from_str(&execution_jobs(
        &contexts,
        Some((context_of("test").as_str(), serde_json::json!("skipped"))),
    ))
    .expect("execution jobs");
    jobs.push(serde_json::json!({
        "name": context_of("test"),
        "head_sha": "1111111111111111111111111111111111111111",
        "conclusion": "success",
    }));
    let jobs = scratch.write(
        "jobs.json",
        &serde_json::to_string(&jobs).expect("duplicate jobs"),
    );
    let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);

    assert!(
        !output.status.success()
            && stdout_of(&output).contains("required-gates-executed-status=failed"),
        "a success for another head must not satisfy a skipped candidate: {}",
        stdout_of(&output)
    );
    assert!(
        stdout_of(&output).contains("\"raw_conclusions\":[\"skipped\"]")
            && stdout_of(&output).contains("\"selected_conclusion\":\"skipped\""),
        "the candidate-head result must be diagnosable: {}",
        stdout_of(&output)
    );
}

#[test]
fn terminal_execution_verdict_accepts_duplicate_success() {
    let contexts = declared_contexts();
    let scratch = Scratch::new("duplicate-success");
    let mut jobs: Vec<serde_json::Value> =
        serde_json::from_str(&execution_jobs(&contexts, None)).expect("execution jobs");
    jobs.push(serde_json::json!({
        "name": context_of("test"),
        "head_sha": "0000000000000000000000000000000000000000",
        "conclusion": "success",
    }));
    let jobs = scratch.write(
        "jobs.json",
        &serde_json::to_string(&jobs).expect("duplicate jobs"),
    );
    let output = run_execution_gate(&[("REQUIRED_GATES_EXECUTED_JOBS", jobs.as_str())]);

    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(
        stdout_of(&output).contains("\"raw_conclusions\":[\"success\",\"success\"]"),
        "duplicate successes must remain visible in diagnostics: {}",
        stdout_of(&output)
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
