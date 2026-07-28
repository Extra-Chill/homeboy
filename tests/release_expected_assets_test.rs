//! Deterministic coverage for the release workflow's expected-asset contract
//! (issue #10547).
//!
//! `release.yml`'s `plan` job publishes an `expected-assets` output that two
//! downstream consumers treat as authoritative:
//!
//! * the `host` job bakes it into the `homeboy.draft-adoption` manifest, where
//!   `validate_draft_adoption` compares it to the remote inventory **exactly**;
//! * `verify-published` requires every listed name to exist on the release.
//!
//! cargo-dist uploads its own `dist-manifest.json` to every GitHub Release but
//! never lists it in `.releases[].artifacts[]`. Deriving `expected-assets` from
//! the planned artifacts alone therefore produced a set that is permanently one
//! asset short of what cargo-dist actually publishes, so exact-inventory draft
//! adoption could never succeed and every stranded draft stayed stranded.
//!
//! These tests run the workflow's *own* jq programs against a real cargo-dist
//! plan manifest and assert the result equals the real published inventory of
//! `v0.321.0`, plus the real inventory of the stranded `v0.321.1` draft that
//! recovery has to adopt.

use std::io::Write;
use std::process::{Command, Stdio};

fn release_workflow() -> &'static str {
    include_str!("../.github/workflows/release.yml")
}

/// A real `dist host --steps=create` manifest, reduced to the fields the
/// derivation reads. Captured from homeboy `v0.321.0`.
fn plan_manifest() -> &'static str {
    include_str!("fixtures/release_expected_assets/plan-dist-manifest.json")
}

/// The real asset inventory GitHub reports for the published `v0.321.0`
/// release — the authority the derived set has to reproduce.
fn published_release_assets() -> &'static str {
    include_str!("fixtures/release_expected_assets/published-release-assets.json")
}

/// The real asset inventory of the stranded `v0.321.1` draft that automatic
/// recovery must be able to adopt.
fn stranded_draft_assets() -> &'static str {
    include_str!("fixtures/release_expected_assets/stranded-draft-v0_321_1-assets.json")
}

/// Pull a `NAME="$(jq -c '<program>' ...)"` program out of the workflow text so
/// the test exercises the shipped derivation rather than a copy of it.
fn workflow_jq_program(variable: &str) -> String {
    let marker = format!("{variable}=\"$(jq -c '");
    let line = release_workflow()
        .lines()
        .find(|line| line.trim_start().starts_with(&marker))
        .unwrap_or_else(|| {
            panic!("release.yml no longer derives {variable} with an inline jq program")
        });
    let start = line.find(&marker).expect("marker located above") + marker.len();
    let rest = &line[start..];
    let end = rest
        .rfind('\'')
        .unwrap_or_else(|| panic!("unterminated jq program for {variable}"));
    rest[..end].to_string()
}

fn jq(program: &str, stdin: &str) -> String {
    let mut child = Command::new("jq")
        .arg("-c")
        .arg(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("jq must be installed to verify the release asset derivation; it is a hard dependency of .github/workflows/release.yml");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write jq stdin");
    let output = child.wait_with_output().expect("jq should complete");
    assert!(
        output.status.success(),
        "jq program `{program}` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("jq emits utf-8")
        .trim()
        .to_string()
}

fn sorted_names(json: &str) -> String {
    jq("sort", json)
}

fn planned_artifacts() -> String {
    jq(&workflow_jq_program("PLANNED_ARTIFACTS"), plan_manifest())
}

fn expected_assets() -> String {
    jq(
        &workflow_jq_program("EXPECTED_ASSETS"),
        &planned_artifacts(),
    )
}

#[test]
fn expected_assets_reproduce_the_real_published_cargo_dist_inventory() {
    assert_eq!(
        sorted_names(&expected_assets()),
        sorted_names(published_release_assets()),
        "release.yml's expected-assets must equal every asset cargo-dist uploads to the GitHub Release"
    );
}

#[test]
fn planned_artifacts_alone_are_not_the_published_inventory() {
    // This is the #10547 defect stated as a property: reverting to the
    // planned-artifact set reintroduces a permanently unsatisfiable exact
    // inventory comparison, so guard it explicitly rather than relying on the
    // positive assertion above to notice.
    let planned = sorted_names(&planned_artifacts());
    let published = sorted_names(published_release_assets());
    assert_ne!(
        planned, published,
        "fixture drift: the planned artifact set is supposed to omit dist-manifest.json"
    );
    assert!(
        !planned.contains("dist-manifest.json"),
        "cargo-dist still omits dist-manifest.json from .releases[].artifacts[]; planned={planned}"
    );
    assert!(
        expected_assets().contains("dist-manifest.json"),
        "expected-assets must add cargo-dist's uploaded dist-manifest.json"
    );
}

#[test]
fn the_stranded_v0_321_1_draft_matches_the_expected_inventory_exactly() {
    // `validate_draft_adoption` compares the manifest's expected names to the
    // remote inventory as sets *and* requires equal cardinality. The stranded
    // draft this recovery path exists for must satisfy that comparison.
    let expected = sorted_names(&expected_assets());
    assert_eq!(
        expected,
        sorted_names(stranded_draft_assets()),
        "the stranded v0.321.1 draft must be adoptable under the derived expected-asset set"
    );
    assert_eq!(
        jq("length", &expected),
        "13",
        "cargo-dist publishes 12 planned artifacts plus dist-manifest.json"
    );
}

#[test]
fn the_plan_job_still_fails_closed_when_cargo_dist_plans_no_artifacts() {
    // Adding a constant to the expected set means `expected-assets` is never
    // empty, so the "planned no release assets" guard must be evaluated against
    // the planned artifacts instead — otherwise a plan that produced nothing
    // would silently publish a release expecting only dist-manifest.json.
    let workflow = release_workflow();
    assert!(
        workflow.contains(r#"if [ "$(jq 'length' <<< "${PLANNED_ARTIFACTS}")" -eq 0 ]; then"#),
        "the empty-plan guard must test the planned artifacts, not the augmented expected set"
    );
    assert!(workflow.contains("cargo-dist planned no release assets"));
    assert_eq!(
        jq(
            &workflow_jq_program("PLANNED_ARTIFACTS"),
            r#"{"releases":[]}"#
        ),
        "[]",
        "an empty plan must still derive an empty planned-artifact set so the guard fires"
    );
}

#[test]
fn recovery_records_control_binary_and_release_target_provenance_separately() {
    // Issue #10519: recovery used to execute the stranded tag's own binary, so
    // a publisher fix merged after the tag could never repair it. The control
    // binary now comes from `gate-build` at `github.sha` while the release
    // target stays pinned to the tag; the run has to state both.
    let workflow = release_workflow();
    assert!(
        workflow.contains("CONTROL_SHA: ${{ github.sha }}"),
        "the finalizer must record which commit built the control binary"
    );
    assert!(workflow.contains("| control binary | "));
    assert!(workflow.contains("| release target tag | "));
    assert!(workflow.contains("| release target commit | "));
    assert!(workflow.contains("| artifact provenance | "));
    assert!(
        workflow.contains(r#"if [ "${CONTROL_SHA}" = "${TARGET_SHA}" ]; then"#),
        "recovery must warn when the control binary cannot bootstrap a post-tag publisher fix"
    );
}
