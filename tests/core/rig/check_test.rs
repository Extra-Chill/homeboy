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

#[test]
fn test_evaluate_newer_than_left_newer_passes() {
    use crate::rig::spec::{NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let older = tmp_dir.path().join("older.txt");
    let newer = tmp_dir.path().join("newer.txt");
    std::fs::write(&older, "x").expect("write");
    // Sleep a beat so mtimes are distinguishable at second granularity.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&newer, "x").expect("write");

    let rig = minimal_rig();
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource {
                file_mtime: Some(newer.to_string_lossy().into_owned()),
                ..Default::default()
            },
            right: TimeSource {
                file_mtime: Some(older.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect("newer left passes");
}

#[test]
fn test_evaluate_newer_than_left_older_fails() {
    use crate::rig::spec::{NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let older = tmp_dir.path().join("older.txt");
    let newer = tmp_dir.path().join("newer.txt");
    std::fs::write(&older, "x").expect("write");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&newer, "x").expect("write");

    let rig = minimal_rig();
    // left = older, right = newer ⇒ check fails.
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource {
                file_mtime: Some(older.to_string_lossy().into_owned()),
                ..Default::default()
            },
            right: TimeSource {
                file_mtime: Some(newer.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    let err = evaluate(&rig, &spec).expect_err("older left fails");
    assert!(err.message.contains("not newer"));
}

#[test]
fn test_evaluate_newer_than_missing_left_process_passes() {
    use crate::rig::spec::{DiscoverSpec, NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let bundle = tmp_dir.path().join("bundle.js");
    std::fs::write(&bundle, "x").expect("write");

    let rig = minimal_rig();
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource {
                process_start: Some(DiscoverSpec {
                    // Pattern that cannot match any process — ensures None.
                    pattern: "homeboy-test-marker-no-process-matches-this-XQZ-9999".to_string(),
                }),
                ..Default::default()
            },
            right: TimeSource {
                file_mtime: Some(bundle.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    // Left is None ⇒ no stale daemon to flag ⇒ pass.
    evaluate(&rig, &spec).expect("absent left process passes");
}

#[test]
fn test_evaluate_newer_than_rejects_empty_time_source() {
    use crate::rig::spec::{NewerThanSpec, TimeSource};
    let rig = minimal_rig();
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource::default(),
            right: TimeSource::default(),
        }),
        ..Default::default()
    };
    let err = evaluate(&rig, &spec).expect_err("empty source rejected");
    assert!(err.message.contains("must specify one of"));
}

#[test]
fn test_evaluate_check_with_no_probe_set_lists_newer_than() {
    let rig = minimal_rig();
    let err = evaluate(&rig, &CheckSpec::default()).expect_err("empty rejected");
    // Documentation drift sentinel — make sure the error names every probe
    // so users see `newer_than` in the suggestion.
    assert!(err.message.contains("newer_than"));
}
