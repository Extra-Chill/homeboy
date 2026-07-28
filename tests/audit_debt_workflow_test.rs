//! Guards the post-merge `main` gate defined in `.github/workflows/audit-debt.yml`.
//!
//! These assertions exist because the gate's value is entirely in its scope. On
//! 2026-07-27 a POSIX-sh portability bug merged green, turned `main` red in
//! `homeboy-lab-runner`'s prune tests, and stayed hidden until a later merge
//! dragged those tests into the release's `--changed-since` window — stranding
//! 188 commits and two tags. Re-narrowing these gates would silently restore
//! that blind spot, so the scope properties are asserted, not just documented.

fn main_guard_workflow() -> &'static str {
    include_str!("../.github/workflows/audit-debt.yml")
}

/// Compile-time proof that the attribution script the workflow invokes exists.
fn report_red_main_script() -> &'static str {
    include_str!("../.github/report-red-main.sh")
}

/// Compile-time proof that the blocking corpus assertion the workflow invokes
/// exists. The tests below then EXECUTE it — see
/// `assert_audit_corpus_rejects_the_historical_audit_json_filename`.
fn audit_corpus_script() -> &'static str {
    include_str!("../.github/assert-audit-corpus.sh")
}

/// A real `homeboy review audit --profile=full --output <path>` payload,
/// recorded from a full run against this repository. The bulk arrays are
/// truncated; the envelope is verbatim. Fixtures that are hand-written to match
/// what a test expects prove nothing — this one is the actual contract the gate
/// has to parse.
fn recorded_review_audit_payload() -> &'static str {
    include_str!("fixtures/audit_corpus/review-audit.json")
}

/// A real payload from a `review audit` that failed BEFORE producing an audit
/// (a stale baseline row on `main`, 2026-07-28). There is no `.data` at all.
fn recorded_failed_review_audit_payload() -> &'static str {
    include_str!("fixtures/audit_corpus/review-audit-failed-validation.json")
}

/// The shape of the `review` UMBRELLA command (audit + lint + test stages),
/// which is where `.data.audit.output.summary` actually lives. `review audit`
/// on its own never produces this. #10583 pointed the gate's jq path here.
const UMBRELLA_REVIEW_PAYLOAD: &str = r#"{
  "schema": "homeboy/command-result/v3",
  "command": "review",
  "success": true,
  "exit_code": 0,
  "data": {
    "audit": { "output": { "summary": { "files_scanned": 1596 } } },
    "lint": { "output": {} },
    "test": { "output": {} }
  }
}
"#;

struct CorpusAssertion {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl CorpusAssertion {
    fn output(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Run the real gate script the workflow runs, against a directory we control.
fn run_corpus_assertion(
    output_dir: Option<&std::path::Path>,
    min_files: Option<&str>,
) -> CorpusAssertion {
    assert!(
        std::process::Command::new("jq")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false),
        "jq must be installed to exercise the audit corpus gate. Skipping silently would make this \
         test the very thing it exists to prevent: a check that reports success while measuring nothing."
    );

    let mut command = std::process::Command::new("bash");
    command.arg(".github/assert-audit-corpus.sh");
    command.env_remove("HOMEBOY_OUTPUT_DIR");
    command.env_remove("AUDIT_MIN_FILES_SCANNED");
    if let Some(dir) = output_dir {
        command.env("HOMEBOY_OUTPUT_DIR", dir);
    }
    if let Some(min) = min_files {
        command.env("AUDIT_MIN_FILES_SCANNED", min);
    }

    let output = command
        .output()
        .expect("corpus assertion script should run");

    CorpusAssertion {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Write one file into a fresh output dir and run the assertion against it.
fn corpus_assertion_with(file_name: &str, payload: &str) -> (tempfile::TempDir, CorpusAssertion) {
    let dir = tempfile::tempdir().expect("output dir");
    std::fs::write(dir.path().join(file_name), payload).expect("write payload");
    let result = run_corpus_assertion(Some(dir.path()), None);
    (dir, result)
}

/// Extract a single job block: from `  <name>:` up to the next line at exactly
/// two spaces of indent (the next job key, or a comment introducing it).
///
/// Anchoring on indentation keeps neighbouring jobs' explanatory comments out
/// of the block, so a `contains` assertion cannot pass or fail on prose that
/// belongs to a different job.
fn job(name: &str) -> String {
    let header = format!("  {name}:");
    let mut block = String::new();
    let mut in_job = false;

    for line in main_guard_workflow().lines() {
        if in_job {
            let starts_new_top_level_entry =
                line.starts_with("  ") && !line.starts_with("   ") && !line.trim_start().is_empty();
            if starts_new_top_level_entry {
                break;
            }
            block.push_str(line);
            block.push('\n');
        } else if line == header {
            in_job = true;
        }
    }

    assert!(in_job, "job `{name}` not found in audit-debt.yml");
    block
}

fn release_blocking_gates() -> [(&'static str, String); 2] {
    [
        ("full-lint-gate", job("full-lint-gate")),
        ("full-test-gate", job("full-test-gate")),
    ]
}

#[test]
fn post_merge_gates_run_the_release_blocking_command_set() {
    let workflow = main_guard_workflow();
    assert!(workflow.contains("push:\n    branches: [main]"));

    // release.yml: RELEASE_BLOCKING_COMMANDS default is 'review lint,review test'.
    // Those two commands are what can block a release, so those two are what a
    // post-merge gate has to run to be predictive of release outcome.
    let lint = job("full-lint-gate");
    assert!(lint.contains("commands: review lint"));

    let test = job("full-test-gate");
    assert!(test.contains("commands: review test"));
    // Mirrors release.yml's gate-test: lint is its own job, so don't pay twice.
    assert!(test.contains("--skip-lint"));

    for (name, gate) in release_blocking_gates() {
        assert!(
            gate.contains("if: github.event_name == 'push'"),
            "{name} must be a post-merge gate"
        );
    }
}

#[test]
fn post_merge_gates_run_at_full_scope() {
    for (name, gate) in release_blocking_gates() {
        // The whole point. homeboy-action's `scope: auto` resolves to mode=full
        // on push events; release.yml narrows itself by explicitly passing
        // `--changed-since <before>`. If that flag ever appears here, this gate
        // degrades to the same changed-scope blind spot the release already has
        // and stops catching cross-scope breakage.
        assert!(
            !gate.contains("--changed-since"),
            "{name} must run at full scope: --changed-since re-creates the #6807 blind spot"
        );
        // Differential gating only applies to pull_request events, and its
        // "only fail on NEW findings" semantics would let an already-red main
        // stay green here.
        assert!(
            !gate.contains("differential-gating"),
            "{name} must not use differential gating"
        );
    }
}

#[test]
fn post_merge_gates_are_blocking() {
    for (name, gate) in release_blocking_gates() {
        // `continue-on-error` is legitimate on the optional app-token step, but
        // must never cover the quality command itself.
        let occurrences = gate.matches("continue-on-error: true").count();
        assert_eq!(
            occurrences, 1,
            "{name} should mark only the app-token step continue-on-error"
        );

        let action = gate
            .find("uses: Extra-Chill/homeboy-action@v2")
            .unwrap_or_else(|| panic!("{name} must run the homeboy action"));
        let tolerated = gate
            .find("continue-on-error: true")
            .expect("checked above that one exists");
        assert!(
            tolerated < action,
            "{name}: continue-on-error must not apply to the quality command"
        );
    }
}

#[test]
fn post_merge_gates_share_a_single_build() {
    // release.yml builds once in gate-build and hands the binary to its gates.
    // Three independently-compiling gates would triple compile cost on every
    // merge, which at this repo's merge rate is what makes the gate affordable.
    let build = job("gate-build");
    assert!(build.contains("cargo build --release --locked"));
    assert!(build.contains("name: homeboy-binary"));

    for name in ["full-audit-gate", "full-lint-gate", "full-test-gate"] {
        let gate = job(name);
        assert!(
            gate.contains("needs: gate-build"),
            "{name} must reuse the build"
        );
        assert!(
            !gate.contains("cargo build") && !gate.contains("cargo run"),
            "{name} must consume the shared artifact instead of rebuilding"
        );
    }
}

#[test]
fn post_merge_failures_are_attributable_to_a_merge() {
    let script = report_red_main_script();
    // Reports the merge commit and the pushed range so the culprit is bounded
    // even when `cancel-in-progress` collapses a burst of merges into one run.
    assert!(script.contains("MERGE_SHA"));
    assert!(script.contains("compare/"));
    assert!(script.contains("GITHUB_STEP_SUMMARY"));
    // Commit subjects/authors are attacker-influenced, so they must reach the
    // script through the environment rather than workflow interpolation.
    for (name, gate) in release_blocking_gates() {
        assert!(
            gate.contains(".github/report-red-main.sh"),
            "{name} must report attribution on failure"
        );
        assert!(
            gate.contains("if: failure()"),
            "{name} must report only on failure"
        );
        assert!(
            !gate.contains("${{ github.event.head_commit.message }}\n        run:"),
            "{name} must not interpolate commit text into a run: body"
        );
    }
}

#[test]
fn post_merge_gates_never_mutate_the_repository() {
    let workflow = main_guard_workflow();
    // A gate that auto-pushes to `main` on failure would be strictly worse than
    // the breakage it is reporting.
    assert!(!workflow.contains("git push"));
    assert!(!workflow.contains("pr-policy-merge: 'true'"));
    // `autofix` is not a homeboy-action@v2 input at all; passing it only earns
    // an "Unexpected input(s)" warning while reading like an enforced property.
    // Asserted on the YAML key shape (not the bare word) so the comments
    // explaining its absence do not trip this.
    for line in workflow.lines() {
        assert!(
            !line.trim_start().starts_with("autofix:"),
            "workflow passes `autofix`, which is not a homeboy-action@v2 input: {line}"
        );
    }
}

#[test]
fn superseded_post_merge_runs_are_cancelled() {
    let workflow = main_guard_workflow();
    // Push monitoring reports on the current tip and strands nothing, so a
    // superseded run is worthless. An explicit qualification is different: it
    // proves an immutable release candidate and must not be cancelled by a
    // later merge.
    assert!(workflow.contains("cancel-in-progress: ${{ inputs.qualification_sha == '' }}"));
    // Push and sweep are separate groups so a merge train cannot cancel the
    // weekly audit sweep.
    assert!(workflow.contains("group: main-guard-"));
    assert!(workflow.contains("github.event_name == 'push' && 'post-merge' || 'sweep'"));
    assert!(workflow.contains("format('qualification-{0}', inputs.qualification_sha)"));
}

#[test]
fn immutable_qualification_checks_out_and_gates_the_requested_sha() {
    let workflow = main_guard_workflow();

    assert!(workflow.contains("qualification_sha:"));
    assert!(workflow.contains("Release Qualification {0}"));
    assert!(workflow.contains("ref: ${{ inputs.qualification_sha || github.sha }}"));
    for name in [
        "gate-build",
        "full-audit-gate",
        "full-lint-gate",
        "full-test-gate",
    ] {
        let gate = job(name);
        assert!(
            gate.contains("inputs.qualification_sha != ''"),
            "{name} must run for an immutable qualification"
        );
    }
}

/// The audit gate must be able to SEE the tree it claims to gate.
///
/// #10557: the previous version of this test asserted
/// `gate.contains("review audit homeboy --profile=full")` and
/// `!gate.contains("continue-on-error: true")`. Both held, and the gate was
/// still passing in 7 seconds having scanned `files_scanned: 0` of 1817 source
/// files — because invoking the raw binary skips homeboy-action's `Install
/// extension` step, no extension declares `provides.file_extensions`, and the
/// audit corpus comes back empty. The test proved the FLAG; it could not see
/// the EFFECT.
///
/// The effect-level assertion now lives in two places:
///
///   * `crates/homeboy-code-audit/src/engine.rs` hard-errors on an empty
///     corpus (`audit.corpus`), covered by
///     `crates/homeboy-code-audit/src/engine_corpus_test.rs`. That is the
///     durable fix — it holds for every consumer of `review audit`, not just
///     this workflow.
///   * this test, which asserts the two structural properties a YAML file can
///     actually carry: the audit runs through the action that installs the
///     extension, and the job carries an unconditionally-blocking corpus check.
#[test]
fn post_merge_full_audit_gate_can_actually_see_the_tree() {
    let gate = job("full-audit-gate");

    assert!(gate.contains("if: github.event_name == 'push'"));

    // The raw binary has no extension installed, so its corpus is empty. Only
    // the action's `Install extension` step makes fingerprinting possible.
    assert!(
        gate.contains("uses: Extra-Chill/homeboy-action@v2"),
        "the audit gate must run through homeboy-action, which owns the \
         `Install extension` step; invoking the binary directly scans 0 files (#10557)"
    );
    // The defect verbatim: `.homeboy-bin/homeboy review audit ...` in a `run:`
    // step. Checked over every non-comment line so a multi-line `run: |` block
    // cannot smuggle it back.
    for line in gate.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        assert!(
            !line.contains("homeboy review audit"),
            "the audit gate must not invoke the binary directly — that skips the \
             extension install and scans 0 files (#10557): {line}"
        );
    }
    // The audit must reach homeboy through the action's command input, which is
    // the path that has an extension installed.
    assert!(gate.contains("commands: review audit"));
    assert!(gate.contains("args: --profile=full"));
}

/// The corpus check is the part of the gate that blocks, and nothing may
/// tolerate its failure.
#[test]
fn post_merge_full_audit_gate_blocks_on_an_empty_corpus() {
    let gate = job("full-audit-gate");

    let assertion = gate
        .find("name: Assert the audit actually scanned the tree")
        .expect("audit gate must carry a corpus assertion step");
    assert!(
        gate.contains("run: bash .github/assert-audit-corpus.sh"),
        "the corpus assertion must be the executable script, not an inline run: block — inline \
         shell can only be checked by a substring test, which is how this step shipped broken twice"
    );
    // Nothing may re-implement the parse inline: an inline copy would drift away
    // from the fixture-tested script without any test noticing.
    assert!(
        !gate.contains("files_scanned=\"$(jq"),
        "the audit gate must not parse the audit result inline; assert-audit-corpus.sh owns that \
         and is exercised against recorded fixtures"
    );
    assert!(
        !audit_corpus_script().is_empty(),
        "the corpus assertion script must exist"
    );

    // Every `continue-on-error: true` in this job must precede the blocking
    // assertion, so none of them can cover it.
    for (offset, _) in gate.match_indices("continue-on-error: true") {
        assert!(
            offset < assertion,
            "the corpus assertion step must not be covered by continue-on-error"
        );
    }
}

/// The happy path, proven by running the gate over a recorded real payload.
///
/// This is the assertion the whole file is about: not "the workflow mentions
/// files_scanned" but "the shipped script, given the bytes `review audit`
/// actually writes, exits 0 and reports the real corpus size".
#[test]
fn audit_corpus_gate_accepts_a_recorded_review_audit_payload() {
    let (_dir, result) =
        corpus_assertion_with("review-audit.json", recorded_review_audit_payload());

    assert!(
        result.status.success(),
        "the recorded payload must pass the corpus gate:\n{}",
        result.output()
    );
    assert!(
        result.stdout.contains("audit corpus: 1596 file(s)"),
        "the gate must report the corpus size it actually read:\n{}",
        result.output()
    );
}

/// Regression: the gate spent its entire life reading `audit.json`.
///
/// homeboy-action derives the result filename from the command
/// (`command_output_stem "review audit"` -> `review-audit`), so `audit.json`
/// never existed. The assertion failed on every merge from the day it landed,
/// and reported it as "the audit did not run to completion" — blaming the audit
/// for the gate's own bug.
#[test]
fn audit_corpus_gate_rejects_the_historical_audit_json_filename() {
    let (_dir, result) = corpus_assertion_with("audit.json", recorded_review_audit_payload());

    assert!(
        !result.status.success(),
        "a payload under the wrong filename must not pass:\n{}",
        result.output()
    );
    assert!(
        result.stderr.contains("review-audit.json"),
        "the failure must name the file homeboy-action actually writes:\n{}",
        result.output()
    );
    // The directory listing is what turns "no output" into "output under a
    // different name", which is the diagnosis nobody got for a whole day.
    assert!(
        result.stderr.contains("audit.json"),
        "the failure must show what the output directory does contain:\n{}",
        result.output()
    );
}

/// Regression: #10583 repointed the jq path at `.data.audit.output.summary`,
/// which is the `review` UMBRELLA shape. `review audit` alone returns the flat
/// audit output. With the old `// 0` default this misreported a shape mismatch
/// as an empty corpus; the gate must distinguish the two.
#[test]
fn audit_corpus_gate_rejects_the_umbrella_review_shape() {
    let (_dir, result) = corpus_assertion_with("review-audit.json", UMBRELLA_REVIEW_PAYLOAD);

    assert!(
        !result.status.success(),
        "the umbrella shape carries no `review audit` corpus figure and must not pass:\n{}",
        result.output()
    );
    assert!(
        result.stderr.contains(".data.summary.files_scanned"),
        "the failure must name the field `review audit` actually writes:\n{}",
        result.output()
    );
    assert!(
        !result.stdout.contains("audit corpus: 0 file(s)"),
        "a shape mismatch must not be reported as an empty corpus — that conflation is what let \
         the wrong jq path look like a plausible finding:\n{}",
        result.output()
    );
}

/// A `review audit` that died before auditing anything writes a real result
/// file with no `.data`. The gate must fail AND surface the actual cause rather
/// than claiming the audit produced no output.
#[test]
fn audit_corpus_gate_surfaces_a_failed_audit_instead_of_blaming_missing_output() {
    let (_dir, result) =
        corpus_assertion_with("review-audit.json", recorded_failed_review_audit_payload());

    assert!(
        !result.status.success(),
        "a failed audit has no corpus and must not pass:\n{}",
        result.output()
    );
    assert!(
        result.stderr.contains("baselines.audit.known_fingerprints"),
        "the gate must print the audit's own failure summary so the operator sees the real cause:\n{}",
        result.output()
    );
}

#[test]
fn audit_corpus_gate_rejects_an_empty_corpus() {
    let payload =
        recorded_review_audit_payload().replace("\"files_scanned\": 1596", "\"files_scanned\": 0");
    assert_ne!(
        payload,
        recorded_review_audit_payload(),
        "the fixture's corpus figure moved; this test was about to assert nothing"
    );

    let (_dir, result) = corpus_assertion_with("review-audit.json", &payload);

    assert!(
        !result.status.success(),
        "an empty corpus is #10557 verbatim and must fail:\n{}",
        result.output()
    );
}

/// `files_scanned >= 1` cannot tell 1596 files from 3. A corpus that collapses
/// without emptying is the same defect wearing a different number, so the gate
/// carries a sanity floor.
#[test]
fn audit_corpus_gate_rejects_a_collapsed_but_non_empty_corpus() {
    let payload =
        recorded_review_audit_payload().replace("\"files_scanned\": 1596", "\"files_scanned\": 3");
    assert_ne!(
        payload,
        recorded_review_audit_payload(),
        "the fixture's corpus figure moved; this test was about to assert nothing"
    );

    let (_dir, result) = corpus_assertion_with("review-audit.json", &payload);

    assert!(
        !result.status.success(),
        "3 of ~1600 scannable files is a collapsed corpus, not a pass:\n{}",
        result.output()
    );
    assert!(
        result.stderr.contains("sanity floor"),
        "the failure must explain the floor and how to move it deliberately:\n{}",
        result.output()
    );
}

/// Fails closed when it cannot find the audit at all. A gate is allowed to
/// produce a false RED; it is never allowed to produce a false green.
#[test]
fn audit_corpus_gate_fails_closed_without_an_output_directory() {
    let unset = run_corpus_assertion(None, None);
    assert!(
        !unset.status.success(),
        "an unset HOMEBOY_OUTPUT_DIR must fail closed:\n{}",
        unset.output()
    );

    let empty = tempfile::tempdir().expect("output dir");
    let missing = run_corpus_assertion(Some(empty.path()), None);
    assert!(
        !missing.status.success(),
        "an output directory with no audit result must fail closed:\n{}",
        missing.output()
    );
}

/// The floor is configurable, and the gate says which floor it applied — so a
/// future corpus shrink is a deliberate edit rather than a silent softening.
#[test]
fn audit_corpus_gate_reports_the_floor_it_applied() {
    let dir = tempfile::tempdir().expect("output dir");
    std::fs::write(
        dir.path().join("review-audit.json"),
        recorded_review_audit_payload(),
    )
    .expect("write payload");

    let result = run_corpus_assertion(Some(dir.path()), Some("1"));
    assert!(result.status.success(), "{}", result.output());
    assert!(
        result.stdout.contains("floor 1"),
        "the gate must state the floor it enforced:\n{}",
        result.output()
    );

    let raised = run_corpus_assertion(Some(dir.path()), Some("100000"));
    assert!(
        !raised.status.success(),
        "a floor above the corpus must fail:\n{}",
        raised.output()
    );
}

/// The findings verdict is deliberately reporting-only for now. Measured on
/// `main` at 38b3fdf1b (run 30357149676, the first run where the audit gate
/// actually scanned): **224 new findings since baseline** — 185
/// `core_boundary_leak` rows unblinded by #10558's corpus fix plus 39 ordinary
/// drift rows. Baselining them needs the fixed binary and a triage pass over
/// the 185, so the flip is tracked in #10569.
///
/// That state is allowed to exist — but only while it is tracked AND while its
/// size is published on every merge. A warning annotation nobody counts is how
/// a reporting-only verdict quietly becomes permanent.
#[test]
fn reporting_only_audit_verdict_carries_a_follow_up_issue() {
    let gate = job("full-audit-gate");

    if !gate.contains("continue-on-error: true") {
        // The follow-up landed and the verdict is blocking again. Nothing to guard.
        return;
    }

    assert!(
        gate.contains("#10569"),
        "a reporting-only audit verdict must name the issue that flips it back to blocking"
    );
    assert!(
        gate.contains("Reporting-only until"),
        "the reporting-only state must be stated at the step it applies to"
    );
    assert!(
        gate.contains("GITHUB_STEP_SUMMARY"),
        "a reporting-only verdict must publish the size of the debt it is tolerating, not just \
         warn that some exists"
    );
    assert!(
        gate.contains("new_items | length"),
        "the published figure must be the count of findings NEW since the baseline — the number \
         that has to reach zero (or be baselined) before #10569 can flip this to blocking"
    );
}

#[test]
fn scheduled_debt_triage_remains_non_blocking_and_separate() {
    let workflow = main_guard_workflow();

    assert!(workflow.contains("full-audit:\n    name: Full-tree audit → tracking issues"));
    assert!(workflow.contains("if: github.event_name != 'push'"));
    assert!(workflow.contains("auto-issue: 'true'"));
    // The sweep must not be dragged into the post-merge path.
    assert!(job("full-audit").contains("if: github.event_name != 'push'"));
}
