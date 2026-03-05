use homeboy::commands::supports::{run, SupportsArgs};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-supports-{name}-{nanos}"))
}

#[test]
fn test_run() {
    let (output, exit_code) = run(
        SupportsArgs {
            command: "test".to_string(),
            option: "--changed-since".to_string(),
        },
        &homeboy::commands::GlobalArgs {},
    )
    .expect("supports command should run");

    assert!(output.supported);
    assert_eq!(exit_code, 0);
}

#[test]
fn test_supports_known_option() {
    let (output, exit_code) = run(
        SupportsArgs {
            command: "test".to_string(),
            option: "--changed-since".to_string(),
        },
        &homeboy::commands::GlobalArgs {},
    )
    .expect("supports command should run");

    assert!(output.supported);
    assert_eq!(exit_code, 0);
}

#[test]
fn test_rejects_unknown_option_with_known_command() {
    let (output, exit_code) = run(
        SupportsArgs {
            command: "test".to_string(),
            option: "--definitely-unknown".to_string(),
        },
        &homeboy::commands::GlobalArgs {},
    )
    .expect("supports command should run");

    assert!(!output.supported);
    assert_eq!(exit_code, 1);
    assert!(!output.known_options.is_empty());
}

#[test]
fn test_rejects_unknown_command() {
    let (output, exit_code) = run(
        SupportsArgs {
            command: "totally unknown command".to_string(),
            option: "--path".to_string(),
        },
        &homeboy::commands::GlobalArgs {},
    )
    .expect("supports command should run");

    assert!(!output.supported);
    assert_eq!(exit_code, 1);
    assert!(output.hint.is_some());
}

#[test]
fn test_normalize_command() {
    let (output, _exit_code) = run(
        SupportsArgs {
            command: "docs   audit".to_string(),
            option: "--path".to_string(),
        },
        &homeboy::commands::GlobalArgs {},
    )
    .expect("supports command should run");

    assert_eq!(output.command, "docs audit");
}

#[test]
fn test_tmp_dir() {
    let one = tmp_dir("a");
    let two = tmp_dir("b");
    assert_ne!(one, two);

    // ensure helper can produce a path that is creatable/cleanable
    fs::create_dir_all(&one).expect("tmp dir should be creatable");
    assert!(one.exists());
    let _ = fs::remove_dir_all(&one);
}
