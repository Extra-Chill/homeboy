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
    // an "Unexpected input(s)" warning.
    for (name, gate) in release_blocking_gates() {
        assert!(
            !gate.contains("autofix"),
            "{name} passes a non-existent input"
        );
    }
}

#[test]
fn superseded_post_merge_runs_are_cancelled() {
    let workflow = main_guard_workflow();
    // Opposite of release.yml's `cancel-in-progress: false`: this gate reports
    // on the current tip and strands nothing, so a superseded run is worthless.
    assert!(workflow.contains("cancel-in-progress: true"));
    // Push and sweep are separate groups so a merge train cannot cancel the
    // weekly audit sweep.
    assert!(workflow.contains("group: main-guard-"));
    assert!(workflow.contains("github.event_name == 'push' && 'post-merge' || 'sweep'"));
}

#[test]
fn post_merge_full_audit_is_a_blocking_gate() {
    let gate = job("full-audit-gate");

    assert!(gate.contains("if: github.event_name == 'push'"));
    assert!(gate.contains("review audit homeboy --profile=full"));
    assert!(!gate.contains("continue-on-error: true"));
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
