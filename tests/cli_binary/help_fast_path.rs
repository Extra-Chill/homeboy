use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
#[test]
fn shipped_discovery_bypasses_runtime_with_composed_and_extension_commands() {
    let home = tempfile::tempdir().expect("temporary home");
    write_cli_extension(home.path(), "sample-runtime", "sample-cli");

    let extensions = home.path().join(".config/homeboy/extensions");
    std::os::unix::fs::symlink(
        extensions.join("missing-runtime"),
        extensions.join("broken-runtime"),
    )
    .expect("broken extension link");

    for (case, args) in [
        ("long-help", &["--help"][..]),
        ("short-help", &["-h"][..]),
        ("version", &["--version"][..]),
        ("nested-help", &["triage", "--help"][..]),
        ("extension-help", &["sample-cli", "--help"][..]),
    ] {
        let sentinel = home.path().join(format!("{case}-runtime-initialized"));
        let output = Command::new(homeboy_bin())
            .args(args)
            .env_clear()
            .env("HOME", home.path())
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .env("HOMEBOY_TEST_RUNTIME_INITIALIZATION_SENTINEL", &sentinel)
            .output()
            .expect("run shipped Homeboy binary");

        assert!(
            output.status.success(),
            "{} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !sentinel.exists(),
            "{} reached ordinary runtime initialization",
            args.join(" ")
        );

        let stdout = String::from_utf8(output.stdout).expect("discovery output is UTF-8");
        assert!(!stdout.is_empty(), "{} produced no output", args.join(" "));
        if matches!(case, "long-help" | "short-help") {
            assert!(
                stdout.contains("triage"),
                "missing composed command: {stdout}"
            );
            assert!(
                stdout.contains("sample-cli"),
                "missing extension command: {stdout}"
            );
            assert!(
                stdout.contains(
                    "Extension health warning: 1 broken extension link(s): broken-runtime"
                ),
                "missing broken-link health: {stdout}"
            );
        }
    }
}

fn write_cli_extension(home: &Path, id: &str, tool: &str) {
    let extension = home.join(".config/homeboy/extensions").join(id);
    fs::create_dir_all(&extension).expect("extension directory");
    fs::write(
        extension.join(format!("{id}.json")),
        serde_json::json!({
            "name": "Fast-path fixture",
            "version": "0.0.0",
            "cli": {
                "tool": tool,
                "display_name": "Fast-path fixture CLI",
                "command_template": "{{cliPath}} {{args}}"
            }
        })
        .to_string(),
    )
    .expect("extension manifest");
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
