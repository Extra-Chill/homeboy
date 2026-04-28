//! Dispatch-behavior tests for the `bench.default_baseline_rig`
//! auto-upgrade in `commands::bench::run`.
//!
//! `maybe_expand_default_baseline` is the single decision point —
//! covering every row of the behavior matrix here keeps the public
//! `run` dispatcher minimal and the contract auditable from one
//! place.
//!
//! Each test that reads a rig spec from disk does so under the shared
//! isolated-`HOME` guard so parallel rig tests do not race on `paths::homeboy()`.

use super::{maybe_expand_default_baseline, BenchArgs, BenchRunArgs};
use crate::commands::utils::args::{
    BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs,
};
use crate::test_support::with_isolated_home;

/// Write a rig spec JSON under `~/.config/homeboy/rigs/{id}.json` for
/// the duration of an isolated-`HOME` block.
fn write_rig_fixture(home: &tempfile::TempDir, id: &str, json: &str) {
    let dir = home.path().join(".config").join("homeboy").join("rigs");
    std::fs::create_dir_all(&dir).expect("mkdir rigs");
    std::fs::write(dir.join(format!("{}.json", id)), json).expect("write fixture");
}

/// Build a `BenchArgs` with the given `rig` list and baseline / opt-out
/// flag positions. Everything else uses sane defaults — these tests
/// only exercise the dispatch helper, not the full bench workflow.
fn make_args(
    rig: Vec<String>,
    baseline: bool,
    ratchet: bool,
    ignore_default_baseline: bool,
) -> BenchArgs {
    BenchArgs {
        command: None,
        run: BenchRunArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            iterations: 1,
            warmup: None,
            runs: 1,
            shared_state: None,
            concurrency: 1,
            baseline_args: BaselineArgs {
                baseline,
                ignore_baseline: false,
                ratchet,
            },
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            _json: HiddenJsonArgs::default(),
            json_summary: false,
            rig,
            scenario_ids: Vec::new(),
            profile: None,
            ignore_default_baseline,
        },
    }
}

/// JSON for a candidate rig that declares `homeboy-main` as its
/// default baseline. Used as the standard fixture across the matrix.
const CANDIDATE_WITH_BASELINE: &str = r#"{
    "id": "candidate",
    "components": { "homeboy": { "path": "/tmp/homeboy" } },
    "bench": {
        "default_component": "homeboy",
        "default_baseline_rig": "homeboy-main"
    }
}"#;

const CANDIDATE_WITHOUT_BASELINE: &str = r#"{
    "id": "candidate",
    "components": { "homeboy": { "path": "/tmp/homeboy" } },
    "bench": { "default_component": "homeboy" }
}"#;

const CANDIDATE_MATRIX_WITH_BASELINE: &str = r#"{
    "id": "candidate",
    "components": {
        "homeboy": { "path": "/tmp/homeboy" },
        "homeboy-rust": { "path": "/tmp/homeboy-rust" }
    },
    "bench": {
        "components": ["homeboy", "homeboy-rust"],
        "default_baseline_rig": "homeboy-main"
    }
}"#;

const CANDIDATE_SELF_REFERENCE: &str = r#"{
    "id": "candidate",
    "components": { "homeboy": { "path": "/tmp/homeboy" } },
    "bench": { "default_baseline_rig": "candidate" }
}"#;

mod cases {
    use super::*;

    #[test]
    fn test_expansion_rewrites_args_into_two_rig_comparison() {
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_WITH_BASELINE);
            let args = make_args(vec!["candidate".to_string()], false, false, false);
            let expanded = maybe_expand_default_baseline(&args.run)
                .expect("dispatch ok")
                .expect("expansion applied");
            assert_eq!(
                expanded,
                vec!["homeboy-main".to_string(), "candidate".to_string()],
                "baseline must come first (the reference); candidate second"
            );
        });
    }

    #[test]
    fn test_no_expansion_when_default_baseline_unset() {
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_WITHOUT_BASELINE);
            let args = make_args(vec!["candidate".to_string()], false, false, false);
            let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
            assert!(
                result.is_none(),
                "rig without default_baseline_rig must not trigger expansion"
            );
        });
    }

    #[test]
    fn test_no_expansion_when_no_bench_block() {
        with_isolated_home(|home| {
            let candidate_no_bench = r#"{
            "id": "candidate",
            "components": { "homeboy": { "path": "/tmp/homeboy" } }
        }"#;
            write_rig_fixture(home, "candidate", candidate_no_bench);
            let args = make_args(vec!["candidate".to_string()], false, false, false);
            let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
            assert!(
                result.is_none(),
                "rig without bench block must not trigger expansion"
            );
        });
    }

    #[test]
    fn test_ignore_default_baseline_flag_suppresses_expansion() {
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_WITH_BASELINE);
            let args = make_args(vec!["candidate".to_string()], false, false, true);
            let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
            assert!(
                result.is_none(),
                "--ignore-default-baseline must short-circuit before rig::load"
            );
        });
    }

    #[test]
    fn test_baseline_flag_suppresses_expansion() {
        // --baseline implies the user wants a deliberate single-rig run
        // that writes a baseline. Auto-upgrading would silently bless the
        // wrong rig. Even though the spec declares default_baseline_rig,
        // skip expansion.
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_WITH_BASELINE);
            let args = make_args(vec!["candidate".to_string()], true, false, false);
            let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
            assert!(result.is_none(), "--baseline must suppress expansion");
        });
    }

    #[test]
    fn test_ratchet_flag_suppresses_expansion() {
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_WITH_BASELINE);
            let args = make_args(vec!["candidate".to_string()], false, true, false);
            let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
            assert!(result.is_none(), "--ratchet must suppress expansion");
        });
    }

    #[test]
    fn test_multi_rig_user_input_wins_over_spec() {
        // User explicitly listed multiple rigs: do not consult the spec
        // at all (no rig::load), and definitely don't rewrite. Explicit
        // beats implicit.
        let args = make_args(vec!["a".to_string(), "b".to_string()], false, false, false);
        // No fixture written — confirms `rig::load` is never called.
        let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
        assert!(
            result.is_none(),
            "multi-rig user input must short-circuit before any rig::load"
        );
    }

    #[test]
    fn test_component_matrix_suppresses_default_baseline_expansion() {
        // A single-rig component matrix should fan out under one rig-state
        // snapshot. Auto-upgrading it into a cross-rig comparison would
        // change the command into a different axis before the matrix runner
        // sees the spec.
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_MATRIX_WITH_BASELINE);
            let args = make_args(vec!["candidate".to_string()], false, false, false);
            let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
            assert!(
                result.is_none(),
                "component matrix must remain a single-rig dispatch"
            );
        });
    }

    #[test]
    fn test_explicit_component_still_allows_default_baseline_expansion() {
        // Passing a component removes the matrix axis, so the existing
        // default_baseline_rig compatibility path remains valid.
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_MATRIX_WITH_BASELINE);
            let mut args = make_args(vec!["candidate".to_string()], false, false, false);
            args.run.comp.component = Some("homeboy".to_string());
            let expanded = maybe_expand_default_baseline(&args.run)
                .expect("dispatch ok")
                .expect("expansion applied");
            assert_eq!(
                expanded,
                vec!["homeboy-main".to_string(), "candidate".to_string()]
            );
        });
    }

    #[test]
    fn test_empty_rig_list_returns_none() {
        // The bare `bench` (no --rig) path is dispatched in `run` before
        // the expansion helper is consulted — confirm the helper is a
        // no-op for that case so a future caller that flips the order
        // doesn't surprise anyone.
        let args = make_args(Vec::new(), false, false, false);
        let result = maybe_expand_default_baseline(&args.run).expect("dispatch ok");
        assert!(result.is_none(), "empty rig list must not expand");
    }

    #[test]
    fn test_self_reference_loop_is_rejected() {
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_SELF_REFERENCE);
            let args = make_args(vec!["candidate".to_string()], false, false, false);
            let err = maybe_expand_default_baseline(&args.run)
                .expect_err("self-reference must be rejected");
            let msg = format!("{}", err);
            assert!(
                msg.contains("default_baseline_rig"),
                "error message must call out the offending field; got: {}",
                msg
            );
            assert!(
                msg.contains("candidate"),
                "error message must name the offending rig; got: {}",
                msg
            );
            assert!(
                msg.contains("--ignore-default-baseline"),
                "error message must point users at the opt-out flag; got: {}",
                msg
            );
        });
    }

    #[test]
    fn test_self_reference_passes_through_with_opt_out() {
        // A user who deliberately wants to bench the candidate alone can
        // pass --ignore-default-baseline; the helper short-circuits
        // before the self-reference check, so a misshapen spec doesn't
        // block the escape hatch.
        with_isolated_home(|home| {
            write_rig_fixture(home, "candidate", CANDIDATE_SELF_REFERENCE);
            let args = make_args(vec!["candidate".to_string()], false, false, true);
            let result = maybe_expand_default_baseline(&args.run)
                .expect("opt-out short-circuits before self-ref check");
            assert!(result.is_none(), "opt-out must yield single-rig dispatch");
        });
    }

    #[test]
    fn test_missing_candidate_rig_surfaces_load_error() {
        // When the candidate spec doesn't exist on disk, the helper
        // bubbles the rig::load error rather than masking it. Keeps the
        // failure surface for typos consistent with `bench --rig <typo>`
        // pre-PR behavior.
        with_isolated_home(|_home| {
            let args = make_args(vec!["nonexistent-rig".to_string()], false, false, false);
            let err = maybe_expand_default_baseline(&args.run).expect_err("missing rig must error");
            let msg = format!("{}", err);
            assert!(
                msg.to_lowercase().contains("nonexistent-rig")
                    || msg.to_lowercase().contains("not found")
                    || msg.to_lowercase().contains("rig"),
                "error must reference the missing rig; got: {}",
                msg
            );
        });
    }
}
