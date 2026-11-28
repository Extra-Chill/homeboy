#[cfg(unix)]
use homeboy_core::test_support::write_component_registration;
use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use std::fs;

#[cfg(unix)]
#[test]
fn misplaced_release_helpers_do_not_enter_the_planner_or_invoke_providers() {
    let context = HermeticTestContext::new();
    // Register even the helper names as components, so a component-not-found
    // error cannot accidentally keep the invocation out of the batch planner.
    for id in ["fixture", "other", "version", "changelog", "show"] {
        write_component_registration(context.home(), id, context.root());
    }
    let bin = context.root().join("bin");
    fs::create_dir(&bin).expect("create sentinel bin directory");
    let sentinel = context.root().join("provider-invoked");
    for name in ["git", "gh"] {
        homeboy_test_support::write_executable(
            &bin.join(name),
            r#"#!/bin/sh
# Invocation tracking may discover the working directory before dispatch.
if [ "${0##*/} $*" = 'git rev-parse --show-toplevel' ]; then
    exit 1
fi
printf '%s\n' "$0 $*" >> "$RELEASE_PROVIDER_SENTINEL"
exit 97
"#,
            0o755,
        )
        .expect("write release provider sentinel");
    }

    for (argv, helper, replacement) in [
        (
            ["fixture", "version"].as_slice(),
            "version",
            "homeboy release version show fixture",
        ),
        (
            ["fixture", "other", "changelog", "show"].as_slice(),
            "changelog",
            "homeboy release changelog show fixture",
        ),
        // With an intervening option, clap can parse `version` as a
        // subcommand while still retaining the leading positional component.
        (
            ["fixture", "--full", "version", "show"].as_slice(),
            "version",
            "homeboy release version show fixture",
        ),
        (
            ["fixture", "--full", "changelog", "show"].as_slice(),
            "changelog",
            "homeboy release changelog show fixture",
        ),
    ] {
        let output = context
            .command(TestBinary::HomeboyFixture)
            .args(["release", "--dry-run"])
            .args(argv)
            .current_dir(context.root())
            .env("PATH", &bin)
            .env("RELEASE_PROVIDER_SENTINEL", &sentinel)
            .output()
            .expect("run misplaced release helper");

        assert_eq!(output.status.code(), Some(2), "{argv:?}: {output:?}");
        let envelope: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("structured release error");
        let diagnostics = &envelope["diagnostics"];
        assert_eq!(diagnostics["code"], "validation.invalid_argument");
        assert_eq!(diagnostics["details"]["field"], "components");
        assert_eq!(diagnostics["details"]["id"], helper);
        assert_eq!(
            diagnostics["hints"][0]["message"],
            format!("Try: {replacement}")
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("[release] stage:"), "{argv:?}: {stderr}");
        assert!(!sentinel.exists(), "{argv:?} invoked a release provider");
    }
}

#[test]
fn release_help_separates_positional_targets_from_nested_commands() {
    let context = HermeticTestContext::new();
    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(["release", "--help"])
        .output()
        .expect("run release help");
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(
        help.lines().any(|line| line.ends_with("[COMPONENTS]...")),
        "{help}"
    );
    assert!(
        help.lines()
            .any(|line| line.ends_with("release [OPTIONS] <COMMAND>")),
        "{help}"
    );
    assert!(
        !help
            .lines()
            .any(|line| line.contains("[COMPONENTS]...") && line.contains("COMMAND")),
        "{help}"
    );
}

#[test]
fn nested_version_helper_reads_the_requested_component() {
    let context = HermeticTestContext::new();
    let root = context.root().join("fixture");
    fs::create_dir(&root).expect("create fixture directory");
    fs::write(
        root.join("homeboy.json"),
        r#"{"id":"fixture","version_targets":[{"file":"VERSION","pattern":"([0-9.]+)"}]}"#,
    )
    .expect("write component config");
    fs::write(root.join("VERSION"), "4.5.6\n").expect("write version file");

    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(["release", "version", "show", "fixture", "--path"])
        .arg(&root)
        .output()
        .expect("run nested release helper");
    assert!(output.status.success(), "{output:?}");
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("version JSON");
    assert_eq!(envelope["data"]["component_id"], "fixture");
    assert_eq!(envelope["data"]["version"], "4.5.6");
}
