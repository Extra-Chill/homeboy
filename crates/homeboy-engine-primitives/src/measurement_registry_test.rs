//! Structural guard: a gate cannot ship a green verdict without stating how it
//! knows it measured something (#10685).
//!
//! # Why this exists rather than another local patch
//!
//! Four gates were fixed in twelve hours for rendering a verdict they had not
//! earned. Each fix was local, each was correct, and none of them stopped the
//! fifth. The durable part of #10685 is not the shared predicate — it is the
//! moment of enforcement, and the only moment that reliably exists is *when a
//! new green-producing code path is written*.
//!
//! So this test enumerates every non-test source line in `crates/` that
//! constructs a passing verdict, and requires each owning file to appear in
//! [`VERDICT_SITES`] with a declared [`MeasurementBasis`]. Adding a new one and
//! not registering it fails the test with instructions. Registering it forces
//! the author to answer, in reviewable prose, the exact question all four
//! incidents failed to ask: *how does this path know it measured anything?*
//!
//! # The gate layer is not written in Rust (#10741)
//!
//! The paragraph above described this registry's original scope — `crates/`,
//! `.rs` — and that scope was itself the next instance of the bug.
//!
//! #10706 merged roughly ninety minutes after this guard landed. It fixed a
//! textbook instance of the invariant: `git rev-parse --abbrev-ref HEAD`
//! returns the literal string `HEAD` in a detached checkout, the release
//! wrapper read that as "not on main" and exited before evaluating anything,
//! and the workflow laundered the resulting empty `release-version` into
//! `No releasable commits` — skipping every job, concluding green, over 131
//! pending commits. It touched `.github/workflows/release.yml` and
//! `tests/release_workflow_test.rs`. **Zero files under `crates/`.** The guard
//! could not have seen it.
//!
//! So the registry now has two layers, in one file and one vocabulary:
//!
//! * [`VERDICT_SITES`] — Rust, `crates/**/*.rs`, keyed by file.
//! * [`GATE_LAYER_SITES`] — shell and YAML, `.github/**/*.{yml,sh}`, keyed by
//!   **decision** rather than by file, and additionally requiring an executable
//!   fixture. See [`GATE_LAYER_SITES`] for why both of those differ.
//!
//! Keeping them together is deliberate. #10690 exists because four independent
//! local implementations of this invariant had grown up and disagreed with each
//! other; answering the same question a fifth time in a second registry file
//! would be more of the same disease.
//!
//! # What it catches, and what it honestly does not
//!
//! It catches: a new verdict-producing path landing without that question being
//! asked; a migrated site silently dropping its call to the shared predicate
//! (see [`MeasurementBasis::SharedPredicate`]); a registered site being deleted
//! or renamed out from under its registration; and, in the gate layer, a
//! skip-rendering decision that has no test executing its actual shell.
//!
//! It does not catch: a registered site whose declared basis is *wrong*. No
//! source scan can. What it buys is that the claim is written down, attached to
//! the code, and reviewed — which is strictly more than the four incidents had.
//!
//! # Assert the effect, not the command string
//!
//! The post-merge audit gate passed for weeks because its test asserted the
//! command it invoked rather than what that command produced. The Rust half of
//! this file is deliberately only the *registration* half of the guard. The
//! *effect* half — feeding recorded no-measurement fixtures through real gates
//! and asserting they do not come out green — lives next to each gate:
//! `homeboy-extension/src/test/run.rs` and
//! `homeboy-extension/src/test/report.rs`. Both halves are required; neither is
//! sufficient.
//!
//! The gate layer does not get that choice. Registration alone provably would
//! not have caught #10706 — `should-release=false` already existed in
//! `release.yml`, so a file-keyed or even decision-keyed registration would have
//! been sitting there, green, while the laundering happened inside it. What was
//! actually missing was any test that *ran* the decide step; `run_decide_step`
//! in `tests/release_workflow_test.rs` was added **by** #10706, not before it.
//! So a gate-layer site that renders a skip must name a fixture, and
//! [`every_skip_rendering_gate_layer_decision_is_executed_by_a_fixture`]
//! requires that fixture to exist and to actually spawn a shell.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How a verdict-producing site establishes that it measured something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasurementBasis {
    /// Calls the shared `measurement` predicate. The test additionally requires
    /// the file to still reference it, so a migration cannot be silently
    /// reverted.
    SharedPredicate,
    /// Projects a verdict that was already decided elsewhere — an exit code, a
    /// remote check conclusion, an enum-to-string rendering, a `From` impl.
    /// These sites cannot measure and must not pretend to; the obligation sits
    /// with whoever produced the value being projected.
    Projection,
    /// Evaluates one unit at a time and emits one verdict per unit, so an empty
    /// input produces an empty result set rather than a green. There is no
    /// aggregate `pass` to manufacture.
    PerUnitEvaluation,
    /// Renders `passed` for a path that explicitly did not run, and carries a
    /// machine-readable marker saying so alongside it (`skipped: true`,
    /// `ran: false`, a `skipped_reason`). Honest, because the absence is
    /// reported rather than hidden.
    ExplicitlySkipped,
    /// An independently-established empty population: something other than the
    /// instrument confirms there was nothing to measure.
    EmptyPopulation,
    /// Not a gate. The literal is a field name, a progress label, or another
    /// non-verdict use that happens to match the scan.
    NotAGate,
    /// Known-unguarded and deliberately left alone, with the reason and the
    /// blast radius recorded in `note`. Registering something here is not
    /// approval — it is a debt marker that a reader can find.
    Unguarded,
}

struct VerdictSite {
    /// Workspace-relative path, `/`-separated.
    file: &'static str,
    basis: MeasurementBasis,
    note: &'static str,
}

/// Every non-test file in `crates/` that constructs a passing verdict.
///
/// Sorted by path. Keep it that way; the failure message is easier to act on.
const VERDICT_SITES: &[VerdictSite] = &[
    VerdictSite {
        file: "crates/contracts/homeboy-extension-contract/src/runner_contract.rs",
        basis: MeasurementBasis::Projection,
        note: "phase_status_from_exit_code: a pure exit-code projection. It is the single \
               most-called green-producer in the tree and it deliberately knows nothing about \
               counts, which is why every caller has to establish measurement itself.",
    },
    VerdictSite {
        file: "crates/contracts/homeboy-gate-contract/src/proof.rs",
        basis: MeasurementBasis::Projection,
        note: "gate_status_label: renders an already-decided HomeboyGateStatus as a string.",
    },
    VerdictSite {
        file: "crates/homeboy-agents/src/agent_task_finalization.rs",
        basis: MeasurementBasis::Projection,
        note: "gate_result_from_legacy: maps a legacy status string onto HomeboyGateStatus. The \
               verdict arrives already made.",
    },
    VerdictSite {
        file: "crates/homeboy-agents/src/agent_task_gate.rs",
        basis: MeasurementBasis::Projection,
        note: "From<AgentTaskGateStatus>, plus accept_inherited_failure, which renders Passed on \
               purpose when a candidate failure provably matches the immutable baseline. That is \
               a measured comparison, not an absence of one.",
    },
    VerdictSite {
        file: "crates/homeboy-cli/src/commands/trace/guardrails.rs",
        basis: MeasurementBasis::PerUnitEvaluation,
        note: "One TraceGuardrailOutput per declared guardrail, from a real evaluate() call. Zero \
               guardrails yields zero outputs, never a green aggregate.",
    },
    VerdictSite {
        file: "crates/homeboy-code-audit/src/run.rs",
        basis: MeasurementBasis::EmptyPopulation,
        note: "The --changed-since shortcut: run_audit returns None only when the git diff itself \
               is empty. Git is the independent population source, so files_scanned: 0 here is an \
               honest zero. Its twin -- a zero corpus against a NON-empty tree -- is the \
               SharedPredicate site in engine.rs.",
    },
    VerdictSite {
        file: "crates/homeboy-core/src/git/pr_land.rs",
        basis: MeasurementBasis::Projection,
        note: "Renders a remote check conclusion. The measurement happened on the forge.",
    },
    VerdictSite {
        file: "crates/homeboy-core/src/validation_progress.rs",
        basis: MeasurementBasis::Unguarded,
        note: "completed_count == command_count renders `passed`, which is also true when \
               command_count is 0. Left as-is deliberately: this is a progress record for \
               operator display, it gates nothing, and every constructor in the tree passes a \
               non-empty command list. Recorded here so that stops being true silently.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/bench/gate.rs",
        basis: MeasurementBasis::Projection,
        note: "normalized_gate_result_for_scenario projects BenchGateResult::passed, which the \
               metric comparison already decided.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/lint/report.rs",
        basis: MeasurementBasis::NotAGate,
        note: "from_lint_fix: the --fix dispatch. Autofixable findings never fail a run by \
               contract (#1459/#1507), so this path returns exit 0 unconditionally and asserts \
               nothing about the tree's health.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/lint/run/findings.rs",
        basis: MeasurementBasis::EmptyPopulation,
        note: "mark_zero_finding_producers_passed rewrites a producer to passed only under four \
               simultaneous conditions -- findings WERE produced, the changed-file filter removed \
               all of them, exit was exactly 1, and no producers were declared. The producer ran \
               and its findings were scoped out; nothing is being inferred from silence.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/lint/run/workflow.rs",
        basis: MeasurementBasis::Unguarded,
        note:
            "Two greens. The changed-file early exit is EmptyPopulation-shaped, and #10685 gave it \
               ScopedLintPlan::changed_files_considered so `nothing changed` and `47 files changed \
               and matched no lint route` stop rendering identically. The other -- \
               `status = if exit_code == 0` -- is downstream of effective_lint_exit_code, which \
               rewrites a non-zero lint exit to zero whenever drift did not increase. That is \
               UNGUARDED: an empty findings set against a non-empty baseline reads as `drift \
               reduced` and renders green. See the doc comment on effective_lint_exit_code for \
               why it is recorded rather than patched here.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/test/parsing.rs",
        basis: MeasurementBasis::NotAGate,
        note: "A parse-spec field NAME that happens to be the string `passed`.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/test/report.rs",
        basis: MeasurementBasis::Projection,
        note: "test_phase_report/test_phase_failure render the status test_run_status already \
               decided, and add the distinctions a reader needs: timeout (#10644) vs zero \
               executed tests vs real failures.",
    },
    VerdictSite {
        file: "crates/homeboy-extension/src/test/run.rs",
        basis: MeasurementBasis::SharedPredicate,
        note: "test_run_status, which had the best local version of this rule before #10685 and \
               supplied its semantics. Also the #8340 changed-scope guard: zero tests selected \
               against a non-empty impacted-source set is Contradicted, a hard error.",
    },
    VerdictSite {
        file: "crates/homeboy-fuzz/src/evidence_contract.rs",
        basis: MeasurementBasis::Projection,
        note: "Enum-to-string rendering of an already-decided evidence status.",
    },
    VerdictSite {
        file: "crates/homeboy-lab-runner/src/capabilities.rs",
        basis: MeasurementBasis::PerUnitEvaluation,
        note: "gate_result_from_lab_decision: Eligible is produced by a preflight that inspected \
               the runner's declared tools. The inspection is the measurement.",
    },
    VerdictSite {
        file: "crates/homeboy-refactor/src/auto/verify.rs",
        basis: MeasurementBasis::ExplicitlySkipped,
        note: "VerifyOutcome::skipped sets passed: true AND skipped: true, so a consumer can tell \
               a no-op apart from a verified apply.",
    },
    VerdictSite {
        file: "crates/homeboy-review/src/review/mod.rs",
        basis: MeasurementBasis::ExplicitlySkipped,
        note: "stage_skipped sets passed: true alongside ran: false and a skipped_reason.",
    },
    VerdictSite {
        file: "crates/homeboy-rig/src/pipeline/lifecycle_step.rs",
        basis: MeasurementBasis::PerUnitEvaluation,
        note: "One LifecyclePhaseResult per executed op, from that op's own Result. No phases \
               executed means no phases recorded.",
    },
];

/// Files that guard a verdict with the shared predicate without themselves
/// containing a green-verdict literal.
///
/// The audit engine is the clearest case: it decides whether an audit may
/// return at all, while the `passed:` field is constructed one layer up in
/// `run.rs`. The guard is the load-bearing part and it would be invisible to a
/// literal scan, so it is registered explicitly and asserted directly.
const SHARED_PREDICATE_SITES: &[(&str, &str)] = &[(
    "crates/homeboy-code-audit/src/engine.rs",
    "#10557/#10574: files_scanned 0 of 1817 rendered passed: true for weeks. Now \
     Measurement::units(0).against_population(walked + unclaimed); a non-empty population makes \
     it Contradicted, which is the one outcome that is a hard error rather than an unknown.",
)];

/// Literals that construct a passing verdict.
///
/// Deliberately narrow. Broadening this to every occurrence of the word
/// `passed` matches 94 files, most of them assertions, and a guard nobody can
/// keep green is a guard nobody keeps.
const GREEN_VERDICT_LITERALS: &[&str] = &[
    "HomeboyGateStatus::Passed",
    "PhaseStatus::Passed",
    "LifecyclePhaseStatus::Passed",
    "passed: true",
    "status: \"passed\"",
    "\"passed\".to_string()",
    "return \"passed\"",
    "=> \"passed\"",
];

/// Substring the [`MeasurementBasis::SharedPredicate`] sites must still contain.
const SHARED_PREDICATE_MARKER: &str = "measurement::";

// ─────────────────────────────────────────────────────────────────────────────
// The shell/YAML gate layer (#10741)
// ─────────────────────────────────────────────────────────────────────────────

/// A verdict-producing decision in the shell/YAML gate layer.
///
/// Keyed by `(file, decision)` rather than by file, unlike [`VerdictSite`].
/// `release.yml` is 1,377 lines and contains nine distinct boolean gating
/// decisions; one registry row covering the whole file would be a row that says
/// nothing, which is the failure mode this registry exists to prevent.
struct GateLayerSite {
    /// Workspace-relative path, `/`-separated.
    file: &'static str,
    /// The exact literal the scan matches: a boolean `$GITHUB_OUTPUT` write, or
    /// a shell script's terminal `exit`.
    decision: &'static str,
    basis: MeasurementBasis,
    /// `true` when this decision renders a **skip** — a value that causes
    /// downstream jobs not to run while the workflow still concludes green.
    ///
    /// This is the #10706 shape, and it is the field that carries the fixture
    /// obligation. It is keyed on the decision's *value*, not on `basis`, so it
    /// cannot be dodged by relabelling a site `Projection`.
    renders_skip: bool,
    /// A test function that executes this decision's real shell and asserts what
    /// it produced. `None` is permitted only with a `NO FIXTURE:` note.
    fixture: Option<&'static str>,
    note: &'static str,
}

/// Every boolean gating decision in `.github/`, plus every gate script's exit.
///
/// Sorted by `(file, decision)`. Keep it that way.
const GATE_LAYER_SITES: &[GateLayerSite] = &[
    GateLayerSite {
        file: ".github/detect-stranded-release.sh",
        decision: "exit 0",
        basis: MeasurementBasis::ExplicitlySkipped,
        renders_skip: false,
        fixture: None,
        note: "NO FIXTURE: `finish` is the script's single exit and it always writes all four \
               outputs, so a crash leaves them unset and release.yml's fallback supplies inert \
               defaults. The one genuine no-measurement path -- `gh release list` failing -- \
               already refuses to read its own failure as `nothing is published`, warns, and \
               finishes empty (line 114). Its remaining soft spot is the empty `git tag --list` \
               case: a shallow or tagless checkout is indistinguishable from a repo with no \
               stranded tags, and both render `stranded-tag=`. That degrades to `no recovery`, \
               never to a false release, so it is recorded rather than patched. Exercising it \
               needs a fixture git repo plus a `gh` stub, which is why there is no fixture yet.",
    },
    GateLayerSite {
        file: ".github/release-quality-policy.sh",
        decision: "exit \"${failed}\"",
        basis: MeasurementBasis::SharedPredicate,
        renders_skip: false,
        fixture: Some("release_quality_policy_refuses_a_blocking_set_that_matches_nothing"),
        note: "#10741, the sixth instance. Three `check_command` calls each fell through to the \
               non-blocking branch when nothing matched, leaving `failed` at 0, so the policy \
               exited green having enforced nothing. `assess_measurement` now ports \
               Measurement::assess to bash: units>0 measured, units==0 with an empty configured \
               population is an honest zero (warn, pass), units==0 against a non-empty population \
               is Contradicted and a hard error.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "blocked=false",
        basis: MeasurementBasis::EmptyPopulation,
        renders_skip: false,
        fixture: None,
        note: "NO FIXTURE: the marker file was read and did not name this HEAD. The filesystem is \
               the independent population source and a missing marker is a real, honest zero -- \
               there is no prior failure to find. This is the permissive direction (it lets the \
               release proceed to real gates), so a defect here cannot manufacture a green skip.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "blocked=true",
        basis: MeasurementBasis::ExplicitlySkipped,
        renders_skip: true,
        fixture: None,
        note: "NO FIXTURE: skips because the restored cache marker holds this exact HEAD SHA. The \
               evidence is a positive string equality against a file this job read, not an \
               absence, so there is no emptiness to launder -- an unreadable or missing marker \
               falls through to `blocked=false`. Debt: no test executes this step; it needs a \
               RUNNER_TEMP fixture.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "prepared=true",
        basis: MeasurementBasis::Projection,
        renders_skip: false,
        fixture: None,
        note: "NO FIXTURE: projects the outcome of the `homeboy release` invocation that ran in \
               the same step. The measurement obligation belongs to core's release planner, not \
               to this echo.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "recovery-release=false",
        basis: MeasurementBasis::Projection,
        renders_skip: false,
        fixture: None,
        note: "NO FIXTURE: a mode flag, not a verdict. It records that the bump type core \
               reported was not `recovery`; the release still proceeds through every gate.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "recovery-release=true",
        basis: MeasurementBasis::Projection,
        renders_skip: true,
        fixture: None,
        note: "NO FIXTURE: recovery legitimately bypasses the quality gates, so this genuinely is \
               a skip-rendering decision. It is a projection of an already-established fact -- an \
               explicit dispatch `release_tag`, a stranded tag the detector selected, or core \
               reporting `bump-type=recovery` -- and each of those three is a positive finding \
               rather than an absence. Debt: the branch selection is covered by \
               `release_check_*` string assertions, not by an executing fixture.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "released=true",
        basis: MeasurementBasis::Projection,
        renders_skip: false,
        fixture: None,
        note:
            "NO FIXTURE: projects the result of the release step that just ran in this job. Same \
               reasoning as `prepared=true`.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "should-release=false",
        basis: MeasurementBasis::EmptyPopulation,
        renders_skip: true,
        fixture: Some("release_check_never_reports_nothing_to_release_without_measuring"),
        note: "**The #10706 site, and the reason this layer exists.** An empty release-version \
               used to render this unconditionally, collapsing a measured negative (core \
               evaluated the commit range and declined) with an unmeasured one (the wrapper bailed \
               before core ran). #10706 split them: only the three reasons core emits from \
               planning_policy.rs may skip, plus a supersession this job proved itself; anything \
               else is UNKNOWN and fails loudly. The population is core's own commit-range \
               evaluation, which is independent of this step.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "should-release=true",
        basis: MeasurementBasis::Projection,
        renders_skip: false,
        fixture: Some("release_check_never_reports_nothing_to_release_without_measuring"),
        note: "Projects a release core already planned, or a tag the recovery/stranded paths \
               already validated. Registered with a fixture anyway because it shares the decide \
               step's branch ladder with `should-release=false`: a fixture that only pinned the \
               skip branch could be satisfied by a step that never releases at all.",
    },
    GateLayerSite {
        file: ".github/workflows/release.yml",
        decision: "superseded=true",
        basis: MeasurementBasis::EmptyPopulation,
        renders_skip: true,
        fixture: None,
        note:
            "NO FIXTURE: the attach step compared HEAD against origin/<release-branch> and found \
               a newer tip, naming the commit whose own run owns the release. That is a positive \
               measurement made in this step, which is precisely why the decide step is allowed \
               to accept `wrong-branch` only when this flag is set -- the narrow exemption that \
               keeps #10706's hard failure intact. Debt: exercising it needs a fixture git repo \
               with an origin remote.",
    },
];

/// Extensions under `.github/` that can carry verdict logic.
///
/// # Why the scan stops at `.github/`
///
/// `crates/homeboy-extension/src/runtime/*.sh` is the other body of shell in
/// this repository, and it was swept for this issue rather than assumed
/// harmless. Those files are evidence *producers*, not verdict renderers: they
/// write the counts and findings sidecars that Rust gates then read. Registering
/// them would add thirteen rows that all say `NotAGate`, burying the eleven that
/// mean something.
///
/// One of them is worth recording anyway. `write-test-results.sh` reads its
/// counts as `local passed="${2:-0}"`, so an *absent* argument is written to the
/// sidecar as a measured `0` — the exact measured-zero/no-measurement collapse
/// this registry is about, at the point where the evidence is created. It does
/// not become a green: `test_run_status` maps `Some(counts)` to
/// `Measurement::units(passed + failed)`, and `units(0)` with no population is
/// `Unmeasured(ZeroUnits)`, which forbids a pass. The consequence is narrower
/// than a false verdict and real — an operator reading the sidecar cannot tell
/// "the runner reported zero tests" from "the runner reported nothing" — so it
/// is recorded here rather than patched under a gate-layer issue.
const GATE_LAYER_EXTENSIONS: &[&str] = &["yml", "yaml", "sh"];

/// Substring the shell [`MeasurementBasis::SharedPredicate`] sites must still
/// contain — the bash counterpart of [`SHARED_PREDICATE_MARKER`].
///
/// A shell script cannot `use` the Rust predicate, so the migration is pinned to
/// the name of the function that ports it. Same anti-silent-revert property:
/// deleting the port while keeping the registration fails.
const SHELL_PREDICATE_MARKER: &str = "assess_measurement";

/// The test file that owns gate-layer fixtures.
const GATE_LAYER_FIXTURE_FILE: &str = "tests/release_workflow_test.rs";

/// Proof that the fixture file executes shells rather than string-matching YAML.
///
/// Without this, "has a fixture" degrades into "has a test that greps the
/// workflow" — which is the exact failure the audit gate shipped for weeks.
const GATE_LAYER_FIXTURE_EXECUTES_SHELL: &str = "Command::new(\"bash\")";

fn workspace_root() -> PathBuf {
    // crates/homeboy-engine-primitives -> crates -> <root>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("homeboy-engine-primitives should live at <root>/crates/homeboy-engine-primitives")
        .to_path_buf()
}

/// Test-owned sources are out of scope: a fixture asserting `passed: true` is
/// the point of the fixture. The rule is by path so it is mechanical and
/// reviewable rather than a judgement call per file.
fn is_test_owned(relative: &str) -> bool {
    relative.contains("/tests/")
        || relative.ends_with("/tests.rs")
        || relative.ends_with("_test.rs")
        || relative.ends_with("_tests.rs")
        || relative.ends_with("test_support.rs")
        || relative.contains("/fixtures/")
}

/// Production lines of a file: everything before the first `#[cfg(test)]`.
///
/// This mirrors the convention the audit's own source policies use
/// (`ignore_after_line_equals`), and it fails **open** — a verdict hidden below
/// a `#[cfg(test)]` is test code by definition, so skipping it can only make
/// this guard more permissive, never spuriously red.
fn production_lines(contents: &str) -> Vec<&str> {
    contents
        .lines()
        .take_while(|line| line.trim() != "#[cfg(test)]")
        .collect()
}

fn line_constructs_a_green_verdict(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    // A comparison reads a verdict; it does not produce one. `assert` is a test
    // idiom that survives outside `#[cfg(test)]` in a few helper modules.
    if line.contains("==") || line.contains("!=") || line.contains("assert") {
        return false;
    }
    GREEN_VERDICT_LITERALS
        .iter()
        .any(|literal| line.contains(literal))
}

fn rust_sources(root: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![root.join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            found.push((relative, contents));
        }
    }
    found
}

fn discovered_verdict_files(root: &Path) -> BTreeSet<String> {
    rust_sources(root)
        .into_iter()
        .filter(|(relative, _)| !is_test_owned(relative))
        .filter(|(_, contents)| {
            production_lines(contents)
                .iter()
                .any(|line| line_constructs_a_green_verdict(line))
        })
        .map(|(relative, _)| relative)
        .collect()
}

// ── Gate-layer scan ──────────────────────────────────────────────────────────

/// Every `.yml`/`.sh` file under `.github/`, workspace-relative.
fn gate_layer_sources(root: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![root.join(".github")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_gate_layer = path.extension().is_some_and(|extension| {
                GATE_LAYER_EXTENSIONS
                    .iter()
                    .any(|candidate| extension == *candidate)
            });
            if !is_gate_layer {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            found.push((relative, contents));
        }
    }
    found
}

/// The gate-layer decision a line makes, if any.
///
/// Two rules, both mechanical:
///
/// 1. **A boolean `$GITHUB_OUTPUT` write.** `name=true` / `name=false` is a
///    verdict a downstream `if:` reads; `release-version=0.3.2` is data. Keying
///    on the boolean-ness is what separates the two without a hand-curated list
///    of blessed output names, so a *newly invented* gating output is caught.
/// 2. **A shell script's terminal `exit`.** A `.github/*.sh` file is invoked as
///    a gate, so its exit status is its verdict.
///
/// A step's `exit 0` inside a workflow `run:` block is deliberately *not* a
/// decision on its own: in YAML the step's verdict is carried by the outputs it
/// wrote, and every such early exit in this repo is paired with one. Counting
/// both would double-register the same decision.
///
/// **Known blind spot, stated rather than hidden:** a brace group redirected
/// wholesale (`{ echo "x=true"; } >> "$GITHUB_OUTPUT"`) writes a boolean gating
/// output on a line that never mentions `$GITHUB_OUTPUT`, so rule 1 misses it.
/// The one such block in this repo (`release.yml`'s stranded-detector fallback)
/// writes only empty strings, so nothing is missed today.
/// [`the_gate_layer_scan_has_no_unseen_boolean_output_blocks`] fails if a
/// boolean ever appears inside one.
fn gate_layer_decision(relative: &str, line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }

    if trimmed.contains("$GITHUB_OUTPUT") {
        for value in ["true", "false"] {
            let needle = format!("={value}\"");
            if let Some(end) = trimmed.find(&needle) {
                let head = &trimmed[..end];
                // `continue`, not `?`: a bare `?` here would abandon the whole
                // line the moment one of the two values failed to parse, which
                // would silently stop the scan seeing the other.
                let Some(name_start) = head.rfind('"').map(|index| index + 1) else {
                    continue;
                };
                let name = &head[name_start..];
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|character| character.is_ascii_lowercase() || character == '-')
                {
                    return Some(format!("{name}={value}"));
                }
            }
        }
        return None;
    }

    // Rule 2 applies to gate scripts only. Indentation is not consulted: the
    // single exit in `detect-stranded-release.sh` lives inside its `finish`
    // helper, and that is still the script's verdict.
    if relative.ends_with(".sh") && trimmed.starts_with("exit ") {
        return Some(trimmed.trim_end_matches(';').to_string());
    }

    None
}

/// Discovered `(file, decision)` pairs across the whole gate layer.
fn discovered_gate_layer_decisions(root: &Path) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for (relative, contents) in gate_layer_sources(root) {
        for line in contents.lines() {
            if let Some(decision) = gate_layer_decision(&relative, line) {
                found.insert((relative.clone(), decision));
            }
        }
    }
    found
}

fn registered_gate_layer_decisions() -> BTreeSet<(String, String)> {
    GATE_LAYER_SITES
        .iter()
        .map(|site| (site.file.to_string(), site.decision.to_string()))
        .collect()
}

#[test]
fn every_green_verdict_site_declares_how_it_established_measurement() {
    let root = workspace_root();
    let discovered = discovered_verdict_files(&root);
    let registered: BTreeSet<String> = VERDICT_SITES
        .iter()
        .map(|site| site.file.to_string())
        .collect();

    let unregistered: Vec<&String> = discovered.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "these files construct a passing verdict but do not declare how they established a \
         measurement:\n  {}\n\n\
         Add an entry to VERDICT_SITES in \
         crates/homeboy-engine-primitives/src/measurement_registry_test.rs stating the \
         MeasurementBasis and a one-line reason.\n\n\
         This is the question four separate gates failed to ask in twelve hours (#10685): the \
         post-merge audit scanned 0 of 1817 files and reported passed: true; a test child was \
         killed before writing its counts; a differential gate rendered a green check over two \
         identical failures. If the answer is `this cannot measure, it only projects a decision \
         made elsewhere`, that is a legitimate and common answer -- say so with \
         MeasurementBasis::Projection. If the answer is `it does not have one`, use \
         MeasurementBasis::Unguarded and record the blast radius.",
        unregistered
            .iter()
            .map(|file| file.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn no_registration_outlives_the_site_it_describes() {
    let root = workspace_root();
    let discovered = discovered_verdict_files(&root);

    let stale: Vec<&str> = VERDICT_SITES
        .iter()
        .map(|site| site.file)
        .filter(|file| !discovered.contains(*file))
        .collect();

    assert!(
        stale.is_empty(),
        "these VERDICT_SITES entries no longer match a file that constructs a passing verdict:\n  \
         {}\n\nThe file was renamed, deleted, or stopped producing a verdict. Remove or update \
         the entry so the registry keeps describing the tree it claims to describe -- a registry \
         that has drifted is a registry that is not being read.",
        stale.join("\n  ")
    );
}

#[test]
fn a_site_claiming_the_shared_predicate_must_still_call_it() {
    let root = workspace_root();

    let claimed = VERDICT_SITES
        .iter()
        .filter(|site| site.basis == MeasurementBasis::SharedPredicate)
        .map(|site| site.file)
        .chain(SHARED_PREDICATE_SITES.iter().map(|(file, _)| *file));

    let mut checked = 0;
    for file in claimed {
        let contents = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|error| panic!("registered site {file} is unreadable: {error}"));
        assert!(
            contents.contains(SHARED_PREDICATE_MARKER),
            "{file} is registered as a shared-predicate site but no longer references \
             `{SHARED_PREDICATE_MARKER}`.\n\n\
             Either restore the predicate call, or change its registration and say what replaced \
             it. Silently dropping the guard while keeping the claim is the exact shape of the \
             regression #10685 exists to prevent."
        );
        checked += 1;
    }

    assert!(
        checked >= 2,
        "this assertion checked {checked} site(s), so it is close to passing vacuously. If the \
         migration was genuinely reverted, delete this test deliberately rather than letting it \
         quietly measure nothing."
    );
}

#[test]
fn every_registered_site_records_a_reason() {
    for site in VERDICT_SITES {
        assert!(
            site.note.len() > 40,
            "{} is registered with a note too short to be a reason: {:?}. The registry is only \
             worth its cost if the entries are readable by the next person.",
            site.file,
            site.note
        );
        assert!(
            !site.file.is_empty() && site.file.starts_with("crates/"),
            "VERDICT_SITES paths are workspace-relative: {:?}",
            site.file
        );
    }
}

#[test]
fn the_registry_is_sorted_by_path() {
    let files: Vec<&str> = VERDICT_SITES.iter().map(|site| site.file).collect();
    let mut sorted = files.clone();
    sorted.sort_unstable();
    assert_eq!(
        files, sorted,
        "keep VERDICT_SITES sorted by path so the failure messages stay easy to act on"
    );
}

/// The scan is the load-bearing part of this guard, so prove it works rather
/// than assuming it. A guard that silently matches nothing is the same defect
/// class it is guarding against.
#[test]
fn the_scan_itself_measures_something() {
    let root = workspace_root();
    let sources = rust_sources(&root);
    assert!(
        sources.len() > 500,
        "the source walk found only {} .rs files under crates/, which cannot be right; the guard \
         below would pass vacuously",
        sources.len()
    );
    let discovered = discovered_verdict_files(&root);
    assert!(
        discovered.len() >= 15,
        "the verdict scan found only {} green-verdict files, which is fewer than the tree is \
         known to contain. A scan that matches nothing renders every assertion in this file \
         vacuous -- the same absence-of-evidence failure #10685 is about.",
        discovered.len()
    );
}

#[test]
fn the_matcher_distinguishes_producing_a_verdict_from_reading_one() {
    assert!(line_constructs_a_green_verdict(
        "            status: HomeboyGateStatus::Passed,"
    ));
    assert!(line_constructs_a_green_verdict("        passed: true,"));
    // Reads, not constructions.
    assert!(!line_constructs_a_green_verdict(
        "        .filter(|gate| gate.status != HomeboyGateStatus::Passed)"
    ));
    assert!(!line_constructs_a_green_verdict(
        "        .all(|gate| gate.status == HomeboyGateStatus::Passed);"
    ));
    assert!(!line_constructs_a_green_verdict(
        "        assert_eq!(result.status, HomeboyGateStatus::Passed);"
    ));
    // Prose describing the bug is not the bug.
    assert!(!line_constructs_a_green_verdict(
        "    // audit then reported `files_scanned: 0, passed: true` and went green"
    ));
}

#[test]
fn production_lines_stop_at_the_test_module() {
    let source =
        "fn real() -> bool {\n    true\n}\n#[cfg(test)]\nmod tests {\n    passed: true\n}\n";
    let lines = production_lines(source);
    assert_eq!(lines.len(), 3);
    assert!(!lines.iter().any(|line| line.contains("passed: true")));
}

// ─────────────────────────────────────────────────────────────────────────────
// The shell/YAML gate layer (#10741)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn every_gate_layer_decision_declares_how_it_established_measurement() {
    let root = workspace_root();
    let discovered = discovered_gate_layer_decisions(&root);
    let registered = registered_gate_layer_decisions();

    let unregistered: Vec<String> = discovered
        .difference(&registered)
        .map(|(file, decision)| format!("{file}  ->  {decision}"))
        .collect();

    assert!(
        unregistered.is_empty(),
        "these gate-layer decisions render a verdict but do not declare how they established a \
         measurement:\n  {}\n\n\
         Add an entry to GATE_LAYER_SITES in \
         crates/homeboy-engine-primitives/src/measurement_registry_test.rs stating the \
         MeasurementBasis, whether it renders a skip, and a reason.\n\n\
         A boolean written to $GITHUB_OUTPUT is a verdict: a downstream `if:` reads it and \
         decides whether an entire job runs. #10706 is what happens when one of those is rendered \
         from an emptiness nobody checked -- `should-release=false` from an empty release-version \
         that the release wrapper never actually computed, skipping every job over 131 commits \
         while the run concluded green (#10703).\n\n\
         If this decision only projects something already decided elsewhere, say so with \
         MeasurementBasis::Projection. If it renders a skip, set renders_skip: true and name a \
         fixture that EXECUTES its shell -- a test that greps the YAML does not count.",
        unregistered.join("\n  ")
    );
}

#[test]
fn no_gate_layer_registration_outlives_the_decision_it_describes() {
    let root = workspace_root();
    let discovered = discovered_gate_layer_decisions(&root);

    let stale: Vec<String> = registered_gate_layer_decisions()
        .difference(&discovered)
        .map(|(file, decision)| format!("{file}  ->  {decision}"))
        .collect();

    assert!(
        stale.is_empty(),
        "these GATE_LAYER_SITES entries no longer match a decision in the tree:\n  {}\n\nThe \
         workflow step or script was renamed, deleted, or stopped rendering that value. Remove or \
         update the entry -- a registry that has drifted is a registry that is not being read.",
        stale.join("\n  ")
    );
}

/// **The acceptance bar for #10741.**
///
/// Registration on its own provably would not have caught #10706:
/// `should-release=false` already existed in `release.yml`, so a registry row
/// for it would have been sitting there, green, while the laundering happened
/// inside the branch that wrote it. What was actually absent was any test that
/// *ran* the decide step — `run_decide_step` was added **by** #10706.
///
/// So this is the clause with teeth. Every decision that renders a skip must
/// name a fixture, that fixture must exist, and the file holding it must
/// actually spawn a shell. Run against the tree as it stood the hour before
/// #10706, this test fails on `should-release=false`.
///
/// `renders_skip` is keyed on the decision's value, not on `basis`, so the
/// obligation cannot be shed by relabelling a site `Projection`.
#[test]
fn every_skip_rendering_gate_layer_decision_is_executed_by_a_fixture() {
    let root = workspace_root();
    let fixtures = std::fs::read_to_string(root.join(GATE_LAYER_FIXTURE_FILE))
        .unwrap_or_else(|error| panic!("{GATE_LAYER_FIXTURE_FILE} is unreadable: {error}"));

    assert!(
        fixtures.contains(GATE_LAYER_FIXTURE_EXECUTES_SHELL),
        "{GATE_LAYER_FIXTURE_FILE} no longer spawns a shell ({GATE_LAYER_FIXTURE_EXECUTES_SHELL}), \
         so every fixture it holds has degraded into a string match against YAML. That is the \
         precise failure the post-merge audit gate shipped for weeks: it asserted the command it \
         ran instead of what that command produced."
    );

    // A skip-rendering decision must either name a fixture or carry the debt
    // marker. `renders_skip` is the obligation; the marker is the escape hatch,
    // and it is greppable on purpose.
    for site in GATE_LAYER_SITES
        .iter()
        .filter(|site| site.renders_skip && site.fixture.is_none())
    {
        assert!(
            site.note.starts_with("NO FIXTURE:"),
            "{} -> {} renders a skip with no fixture, and its note does not open with \
             `NO FIXTURE:`.\n\nA skip-rendering decision without an executing test is exactly the \
             state `should-release=false` was in when #10706 laundered a detached HEAD into \
             `No releasable commits`. If it genuinely cannot be fixtured yet, say so explicitly \
             so the debt is greppable rather than invisible.",
            site.file,
            site.decision
        );
    }

    // Every *claimed* fixture must exist. Counted across all sites, not only
    // the skip-rendering ones: an aspirational fixture name is a false claim
    // wherever it appears.
    let mut executed = 0;
    for site in GATE_LAYER_SITES {
        let Some(fixture) = site.fixture else {
            continue;
        };
        assert!(
            fixtures.contains(&format!("fn {fixture}(")),
            "{} -> {} names fixture `{fixture}`, but no such test exists in \
             {GATE_LAYER_FIXTURE_FILE}.\n\nEither the test was renamed and the registration was \
             not, or the registration was aspirational. Both leave a decision unexercised while \
             claiming otherwise.",
            site.file,
            site.decision
        );
        executed += 1;
    }

    assert!(
        executed >= 2,
        "only {executed} gate-layer decision(s) resolved to a real fixture, so this assertion is \
         close to passing vacuously -- the same absence-of-evidence failure it exists to prevent."
    );

    // And the acceptance case specifically. A general floor can drift until it
    // no longer covers the incident that motivated it; recorded incidents get
    // replayed by name.
    let incident = GATE_LAYER_SITES
        .iter()
        .find(|site| {
            site.file == ".github/workflows/release.yml" && site.decision == "should-release=false"
        })
        .expect("the #10706 decision must stay registered");
    let fixture = incident.fixture.expect(
        "`should-release=false` must keep an executing fixture -- it is the acceptance case",
    );
    assert!(
        fixtures.contains(&format!("fn {fixture}(")),
        "the #10706 acceptance case names fixture `{fixture}`, which does not exist"
    );
}

/// The bash port of the shared predicate must still be there.
///
/// A shell script cannot `use` the Rust `measurement` module, so the migration
/// is pinned to the name of the function that ports it. Same property as
/// [`a_site_claiming_the_shared_predicate_must_still_call_it`]: silently
/// dropping the guard while keeping the claim is the regression shape itself.
#[test]
fn a_gate_layer_site_claiming_the_shared_predicate_must_still_port_it() {
    let root = workspace_root();

    let mut checked = 0;
    for site in GATE_LAYER_SITES
        .iter()
        .filter(|site| site.basis == MeasurementBasis::SharedPredicate)
    {
        let contents = std::fs::read_to_string(root.join(site.file))
            .unwrap_or_else(|error| panic!("registered site {} is unreadable: {error}", site.file));
        assert!(
            contents.contains(SHELL_PREDICATE_MARKER),
            "{} is registered as a shared-predicate site but no longer defines \
             `{SHELL_PREDICATE_MARKER}`.\n\nRestore the port, or change the registration and say \
             what replaced it.",
            site.file
        );
        // The port is worthless if it never classifies an empty population.
        for outcome in ["empty-population", "contradicted"] {
            assert!(
                contents.contains(outcome),
                "{} ports the shared predicate but never renders `{outcome}`. The whole point is \
                 that a measured zero and a broken instrument stop being indistinguishable.",
                site.file
            );
        }
        checked += 1;
    }

    assert_eq!(
        checked, 1,
        "expected exactly one shell shared-predicate site (release-quality-policy.sh); found \
         {checked}. If another script ported the predicate, register it; if this one stopped, \
         this assertion has gone vacuous."
    );
}

#[test]
fn every_registered_gate_layer_site_records_a_reason() {
    for site in GATE_LAYER_SITES {
        assert!(
            site.note.len() > 40,
            "{} -> {} is registered with a note too short to be a reason: {:?}",
            site.file,
            site.decision,
            site.note
        );
        assert!(
            site.file.starts_with(".github/"),
            "GATE_LAYER_SITES paths are workspace-relative under .github/: {:?}",
            site.file
        );
        assert!(
            site.fixture.is_some() || site.note.starts_with("NO FIXTURE:"),
            "{} -> {} has no fixture and does not open its note with `NO FIXTURE:`, so the debt is \
             invisible to a reader grepping for it",
            site.file,
            site.decision
        );
    }
}

#[test]
fn the_gate_layer_registry_is_sorted() {
    let keys: Vec<(&str, &str)> = GATE_LAYER_SITES
        .iter()
        .map(|site| (site.file, site.decision))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(
        keys, sorted,
        "keep GATE_LAYER_SITES sorted by (file, decision) so failure messages stay easy to act on"
    );
}

/// The gate-layer scan is load-bearing, so prove it works rather than assuming
/// it. A scan that silently matches nothing renders every assertion above
/// vacuous — the same defect class the whole registry guards against.
#[test]
fn the_gate_layer_scan_itself_measures_something() {
    let root = workspace_root();

    let sources = gate_layer_sources(&root);
    assert!(
        sources.len() >= 4,
        "the gate-layer walk found only {} .yml/.sh files under .github/, which cannot be right",
        sources.len()
    );
    assert!(
        sources
            .iter()
            .any(|(relative, _)| relative == ".github/workflows/release.yml"),
        "the gate-layer walk did not find release.yml, so it is not walking what it claims to"
    );

    let discovered = discovered_gate_layer_decisions(&root);
    assert!(
        discovered.len() >= 8,
        "the gate-layer scan found only {} decision(s); the tree is known to contain more",
        discovered.len()
    );

    // The #10706 site specifically. Recorded incidents get replayed rather than
    // trusted to a general rule that could quietly stop covering them.
    assert!(
        discovered.contains(&(
            ".github/workflows/release.yml".to_string(),
            "should-release=false".to_string()
        )),
        "the scan no longer sees release.yml's `should-release=false` -- the decision #10706 was \
         about. Either it was renamed (update the registry) or the matcher regressed."
    );
    let incident = GATE_LAYER_SITES
        .iter()
        .find(|site| {
            site.file == ".github/workflows/release.yml" && site.decision == "should-release=false"
        })
        .expect("the #10706 decision must stay registered");
    assert!(
        incident.renders_skip,
        "`should-release=false` skips every downstream job; recording it as anything else \
         releases it from the fixture obligation that is the whole point of this layer"
    );
    assert!(
        incident.fixture.is_some(),
        "`should-release=false` must keep an executing fixture -- it is the acceptance case"
    );
}

#[test]
fn the_gate_layer_matcher_distinguishes_a_verdict_from_data() {
    // Boolean gating outputs are verdicts.
    assert_eq!(
        gate_layer_decision(
            ".github/workflows/release.yml",
            "            echo \"should-release=false\" >> \"$GITHUB_OUTPUT\""
        )
        .as_deref(),
        Some("should-release=false")
    );
    assert_eq!(
        gate_layer_decision(
            ".github/workflows/release.yml",
            "          echo \"superseded=true\" >> \"$GITHUB_OUTPUT\""
        )
        .as_deref(),
        Some("superseded=true")
    );

    // Non-boolean outputs are data, not verdicts. Registering them would bury
    // the verdicts in noise.
    assert!(gate_layer_decision(
        ".github/workflows/release.yml",
        "            echo \"release-version=${RELEASE_VERSION}\" >> \"$GITHUB_OUTPUT\""
    )
    .is_none());
    assert!(gate_layer_decision(
        ".github/workflows/release.yml",
        "              echo \"stranded-attempts=0\" >> \"$GITHUB_OUTPUT\""
    )
    .is_none());

    // Prose describing a decision is not the decision.
    assert!(gate_layer_decision(
        ".github/workflows/release.yml",
        "          # used to write should-release=false into \"$GITHUB_OUTPUT\" unconditionally"
    )
    .is_none());

    // Gate scripts declare a verdict by exiting.
    assert_eq!(
        gate_layer_decision(".github/release-quality-policy.sh", "exit \"${failed}\"").as_deref(),
        Some("exit \"${failed}\"")
    );
    assert_eq!(
        gate_layer_decision(".github/detect-stranded-release.sh", "  exit 0").as_deref(),
        Some("exit 0")
    );
    // A workflow step's early exit is carried by the outputs it wrote.
    assert!(gate_layer_decision(".github/workflows/release.yml", "            exit 0").is_none());
    // `return` is a function's control flow, not the script's verdict.
    assert!(gate_layer_decision(".github/release-quality-policy.sh", "  return 0").is_none());
}

/// The documented blind spot, pinned so it stays a blind spot in theory only.
///
/// A brace group redirected wholesale writes gating outputs on lines that never
/// mention `$GITHUB_OUTPUT`, so the line-oriented matcher cannot see them. The
/// repo has exactly one such block and it writes only empty strings. If a
/// boolean ever appears inside one, this fails and the matcher must grow.
#[test]
fn the_gate_layer_scan_has_no_unseen_boolean_output_blocks() {
    let root = workspace_root();

    for (relative, contents) in gate_layer_sources(&root) {
        let lines: Vec<&str> = contents.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.trim().starts_with('}') || !line.contains("$GITHUB_OUTPUT") {
                continue;
            }
            let start = lines[..index]
                .iter()
                .rposition(|candidate| candidate.trim() == "{")
                .unwrap_or(0);
            for hidden in &lines[start..index] {
                assert!(
                    !hidden.contains("=true\"") && !hidden.contains("=false\""),
                    "{relative} writes a boolean gating output inside a redirected brace group \
                     ({}), which the line-oriented gate-layer matcher cannot see. Teach \
                     gate_layer_decision about brace groups, or write the output with its own \
                     `>> \"$GITHUB_OUTPUT\"`.",
                    hidden.trim()
                );
            }
        }
    }
}
