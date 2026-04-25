//! Check evaluator tests for `src/core/rig/check.rs`.
//!
//! HTTP checks require a reachable endpoint which is fragile in CI; the
//! `file` and `command` probes exercise the full one-of-three logic,
//! short-circuit on validation errors, and cover substring matching.

use crate::rig::check::evaluate;
use crate::rig::spec::{CheckSpec, RigSpec};

fn minimal_rig() -> RigSpec {
    RigSpec {
        id: "t".to_string(),
        description: String::new(),
        components: Default::default(),
        services: Default::default(),
        symlinks: Vec::new(),
        pipeline: Default::default(),
        bench: None,
    }
}

#[test]
fn test_evaluate_rejects_empty_spec() {
    let rig = minimal_rig();
    let err = evaluate(&rig, &CheckSpec::default()).expect_err("empty spec rejected");
    assert!(err.message.contains("must specify one of"));
}

#[test]
fn test_evaluate_rejects_multiple_probes() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        http: Some("http://example.com".to_string()),
        file: Some("/tmp/x".to_string()),
        ..Default::default()
    };
    let err = evaluate(&rig, &spec).expect_err("multiple probes rejected");
    assert!(err.message.contains("must specify exactly one of"));
}

#[test]
fn test_evaluate_file_exists() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let rig = minimal_rig();
    let spec = CheckSpec {
        file: Some(tmp.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect("existing file passes");
}

#[test]
fn test_evaluate_file_missing() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        file: Some("/definitely/does/not/exist/ever-420".to_string()),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect_err("missing file fails");
}

#[test]
fn test_evaluate_file_contains_substring() {
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let path = tmp_dir.path().join("check.txt");
    std::fs::write(&path, "hello world\nsecond line\n").expect("write");
    let rig = minimal_rig();

    let pass = CheckSpec {
        file: Some(path.to_string_lossy().into_owned()),
        contains: Some("world".to_string()),
        ..Default::default()
    };
    evaluate(&rig, &pass).expect("substring present");

    let fail = CheckSpec {
        file: Some(path.to_string_lossy().into_owned()),
        contains: Some("not-in-file".to_string()),
        ..Default::default()
    };
    evaluate(&rig, &fail).expect_err("substring absent");
}

#[test]
fn test_evaluate_command_exit_code_matches() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        command: Some("true".to_string()),
        expect_exit: Some(0),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect("`true` exits 0");
}

#[test]
fn test_evaluate_command_unexpected_exit() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        command: Some("false".to_string()),
        expect_exit: Some(0),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect_err("`false` fails expect_exit=0");
}
