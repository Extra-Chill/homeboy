use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
#[test]
fn shipped_discovery_bypasses_runtime_with_builtin_and_extension_commands() {
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
        ("nested-help", &["status", "--help"][..]),
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
                stdout.contains("status"),
                "missing built-in command: {stdout}"
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

#[test]
fn invalid_help_subcommand_fails_locally_with_upgrade_suggestion() {
    let args = ["homeboy", "update", "--help"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let diagnostic = homeboy::cli_runtime::update_help_diagnostic(&args)
        .expect("update help has a local diagnostic");
    assert!(
        diagnostic.contains("unrecognized subcommand 'update'") && diagnostic.contains("upgrade"),
        "missing upgrade suggestion: {diagnostic}"
    );
}

/// The retired `update --help` spelling must be answered before Homeboy builds
/// its command surface, because that discovers extensions and can block on a
/// damaged installation. A manifest that is a FIFO with no writer blocks any
/// read forever, so reaching extension discovery turns into a hang that the
/// bounded wait reports. The bound only limits how long a regression takes to
/// fail; a correct fast path never opens the FIFO and exits immediately.
#[cfg(unix)]
#[test]
fn invalid_help_subcommand_never_discovers_extensions() {
    use std::os::unix::ffi::OsStrExt;

    let home = tempfile::tempdir().expect("temporary home");
    let extension = home.path().join(".config/homeboy/extensions/stalled");
    fs::create_dir_all(&extension).expect("extension directory");
    let manifest = std::ffi::CString::new(extension.join("stalled.json").as_os_str().as_bytes())
        .expect("FIFO path has no NUL");
    assert_eq!(
        unsafe { libc::mkfifo(manifest.as_ptr(), 0o600) },
        0,
        "create blocking extension manifest: {}",
        std::io::Error::last_os_error()
    );
    let sentinel = home.path().join("runtime-initialized");

    let mut child = Command::new(homeboy_bin())
        .args(["update", "--help"])
        .env_clear()
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_TEST_RUNTIME_INITIALIZATION_SENTINEL", &sentinel)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run shipped Homeboy binary");
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().expect("poll Homeboy").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("update --help blocked reading extension manifests");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("collect Homeboy output");

    assert_eq!(output.status.code(), Some(2), "update --help exit status");
    let stderr = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(
        stderr.contains("unrecognized subcommand 'update'") && stderr.contains("upgrade"),
        "missing upgrade suggestion: {stderr}"
    );
    assert!(
        !sentinel.exists(),
        "update --help reached ordinary runtime initialization"
    );
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
