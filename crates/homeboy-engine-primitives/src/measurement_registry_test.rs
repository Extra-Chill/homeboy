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
//! # What it catches, and what it honestly does not
//!
//! It catches: a new verdict-producing path landing without that question being
//! asked; a migrated site silently dropping its call to the shared predicate
//! (see [`MeasurementBasis::SharedPredicate`]); a registered site being deleted
//! or renamed out from under its registration.
//!
//! It does not catch: a registered site whose declared basis is *wrong*. No
//! source scan can. What it buys is that the claim is written down, attached to
//! the code, and reviewed — which is strictly more than the four incidents had.
//!
//! # Assert the effect, not the command string
//!
//! The post-merge audit gate passed for weeks because its test asserted the
//! command it invoked rather than what that command produced. This file is
//! deliberately the *registration* half of the guard. The *effect* half —
//! feeding recorded no-measurement fixtures through real gates and asserting
//! they do not come out green — lives next to each gate:
//! `homeboy-extension/src/test/run.rs` and
//! `homeboy-extension/src/test/report.rs`. Both halves are required; neither is
//! sufficient.

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
        basis: MeasurementBasis::EmptyPopulation,
        note:
            "The changed-file early exit. #10685 gave it ScopedLintPlan::changed_files_considered \
               so `nothing changed` and `47 files changed and matched no lint route` stop \
               rendering identically. The verdict is deliberately not moved -- see the comment at \
               the call site for why the predicate cannot adjudicate this one.",
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
