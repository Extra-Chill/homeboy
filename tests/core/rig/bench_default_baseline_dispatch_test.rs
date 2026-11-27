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
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, PresentationArgs, SettingArgs,
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
        json: false,
        presentation: PresentationArgs::default(),
        run: BenchRunArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            iterations: 1,
            warmup: None,
            runs: 1,
            run_id: None,
            shared_state: None,
            concurrency: 1,
            matrix: Vec::new(),
            runner_pool: None,
            matrix_max_tasks: None,
            matrix_max_queue_depth: None,
            expected_artifact: Vec::new(),
            baseline_args: BaselineArgs {
                baseline,
                ignore_baseline: false,
                ratchet,
            },
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            json_summary: false,
            status_file: None,
            report: Vec::new(),
            rig,
            rig_order: super::BenchRigOrder::Input,
            rig_concurrency: 1,
            scenario_ids: Vec::new(),
            profile: None,
            ci_profile: None,
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

const CANDIDATE_WITHOUT_BENCH: &str = r#"{
    "id": "candidate",
    "components": { "homeboy": { "path": "/tmp/homeboy" } }
}"#;

/// One row of the dispatch behavior matrix.
struct Case {
    label: &'static str,
    /// Candidate rig spec written to disk; `None` writes nothing, which
    /// proves the helper short-circuits before `rig::load`.
    fixture: Option<&'static str>,
    rigs: &'static [&'static str],
    baseline: bool,
    ratchet: bool,
    ignore_default_baseline: bool,
    component: Option<&'static str>,
}

const CASE: Case = Case {
    label: "",
    fixture: Some(CANDIDATE_WITH_BASELINE),
    rigs: &["candidate"],
    baseline: false,
    ratchet: false,
    ignore_default_baseline: false,
    component: None,
};

fn expand(case: &Case) -> homeboy::core::Result<Option<Vec<String>>> {
    with_isolated_home(|home| {
        if let Some(json) = case.fixture {
            write_rig_fixture(home, "candidate", json);
        }
        let mut args = make_args(
            case.rigs.iter().map(|rig| rig.to_string()).collect(),
            case.baseline,
            case.ratchet,
            case.ignore_default_baseline,
        );
        args.run.comp.component = case.component.map(str::to_string);
        maybe_expand_default_baseline(&args.run)
            .map(|expansion| expansion.map(|expansion| expansion.rig_ids))
    })
}

#[test]
fn test_default_baseline_expands_single_rig_into_baseline_first_comparison() {
    let cases = [
        Case {
            label: "single candidate with default_baseline_rig",
            ..CASE
        },
        // Passing a component removes the matrix axis, so expansion applies.
        Case {
            label: "explicit component on a component-matrix rig",
            fixture: Some(CANDIDATE_MATRIX_WITH_BASELINE),
            component: Some("homeboy"),
            ..CASE
        },
    ];

    for case in cases {
        let rig_ids = expand(&case)
            .unwrap_or_else(|e| panic!("{}: {}", case.label, e))
            .unwrap_or_else(|| panic!("{}: expansion applied", case.label));
        assert_eq!(
            rig_ids,
            vec!["homeboy-main".to_string(), "candidate".to_string()],
            "{}: baseline must come first (the reference); candidate second",
            case.label
        );
    }
}

#[test]
fn test_default_baseline_expansion_is_suppressed() {
    let cases = [
        Case {
            label: "rig without default_baseline_rig",
            fixture: Some(CANDIDATE_WITHOUT_BASELINE),
            ..CASE
        },
        Case {
            label: "rig without bench block",
            fixture: Some(CANDIDATE_WITHOUT_BENCH),
            ..CASE
        },
        Case {
            label: "--ignore-default-baseline",
            ignore_default_baseline: true,
            ..CASE
        },
        // --baseline writes a deliberate single-rig baseline; auto-upgrading
        // would silently bless the wrong rig.
        Case {
            label: "--baseline",
            baseline: true,
            ..CASE
        },
        Case {
            label: "--ratchet",
            ratchet: true,
            ..CASE
        },
        // A component matrix fans out under one rig-state snapshot and must
        // stay a single-rig dispatch.
        Case {
            label: "component matrix",
            fixture: Some(CANDIDATE_MATRIX_WITH_BASELINE),
            ..CASE
        },
        // The opt-out short-circuits before the self-reference check so a
        // misshapen spec does not block the escape hatch.
        Case {
            label: "self-reference with opt-out",
            fixture: Some(CANDIDATE_SELF_REFERENCE),
            ignore_default_baseline: true,
            ..CASE
        },
        // Explicit multi-rig input wins over the spec without loading it.
        Case {
            label: "multi-rig user input",
            fixture: None,
            rigs: &["a", "b"],
            ..CASE
        },
        Case {
            label: "empty rig list",
            fixture: None,
            rigs: &[],
            ..CASE
        },
    ];

    for case in cases {
        let result = expand(&case).unwrap_or_else(|e| panic!("{}: {}", case.label, e));
        assert!(result.is_none(), "{} must not expand", case.label);
    }
}

#[test]
fn test_default_baseline_dispatch_rejects_bad_candidates() {
    let cases = [
        (
            Case {
                label: "self-reference loop",
                fixture: Some(CANDIDATE_SELF_REFERENCE),
                ..CASE
            },
            // Names the field, the rig and the opt-out flag.
            &[
                "default_baseline_rig",
                "candidate",
                "--ignore-default-baseline",
            ][..],
        ),
        // A missing candidate bubbles the rig::load error instead of masking it.
        (
            Case {
                label: "missing candidate rig",
                fixture: None,
                rigs: &["nonexistent-rig"],
                ..CASE
            },
            &["Rig not found"][..],
        ),
    ];

    for (case, fragments) in cases {
        let msg = expand(&case).expect_err(case.label).to_string();
        for fragment in fragments {
            assert!(
                msg.contains(fragment),
                "{}: error must contain `{fragment}`; got: {msg}",
                case.label
            );
        }
    }
}
