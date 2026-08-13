use std::path::PathBuf;
use std::process::{Command, Output};

#[test]
fn historical_cook_executor_flags_emit_the_migration_snapshot() {
    for flag in ["--provider", "--provider-id", "--dispatch-selector"] {
        let output = cook_command([flag, "opencode.agent-task-executor"]);

        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "error: historical executor selection flag '{flag}' is not supported\n\
\nhint: executor selection now uses `--backend <backend>` and optional `--selector <provider-id>`.\n\
Example: `homeboy agent-task cook --backend opencode --selector opencode.agent-task-executor --to-worktree repo@branch --goal 'Describe the task' --verify 'cargo test' --no-finalize`\n\
List available executor providers: `homeboy agent-task providers`\n\
`--provider-argv` is promotion-only: it configures the deprecated promotion apply-provider invocation and cannot select an executor.\n\
\nFor more information, try 'homeboy agent-task cook --help'\n"
            ),
            "{flag}"
        );
    }
}

#[test]
fn cook_accepts_current_executor_selection_flags() {
    for selector_flag in ["--selector", "--dispatch-provider-id"] {
        let output = cook_command([
            "--backend",
            "opencode",
            selector_flag,
            "opencode.agent-task-executor",
            "--help",
        ]);

        assert!(output.status.success(), "{selector_flag}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("--backend <BACKEND>"));
    }
}

#[test]
fn unrelated_unknown_cook_flag_keeps_clap_diagnostic_without_promotion_hint() {
    let output = cook_command(["--unrelated-unknown-flag"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr.contains("unexpected argument '--unrelated-unknown-flag' found"));
    assert!(!stderr.contains("historical executor selection flag"));
    assert!(!stderr.contains("--provider-argv"));
}

fn cook_command<const N: usize>(args: [&str; N]) -> Output {
    Command::new(homeboy_bin())
        .args(["agent-task", "cook"])
        .args(args)
        .env("HOME", tempfile::tempdir().expect("home").path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy")
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
