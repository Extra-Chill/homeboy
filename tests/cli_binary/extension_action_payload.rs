use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

#[test]
fn extension_action_cli_passes_payload_and_selected_rows_from_all_json_specs() {
    homeboy_core::test_support::with_isolated_home(|home| {
        install_fixture_extension(home.path());
        let payload_file = home.path().join("payload.json");
        std::fs::write(
            &payload_file,
            r#"{"nested":{"value":"from-file"},"secret":"do-not-echo"}"#,
        )
        .expect("payload fixture");

        let inline = run_action(
            &[
                "--payload",
                r#"{"nested":{"value":"inline"}}"#,
                "--data",
                r#"[{"id":7}]"#,
            ],
            None,
        );
        assert!(inline.0.success(), "{inline:?}");
        assert!(
            inline.1.contains("inline"),
            "captured output: {:?}",
            inline.1
        );
        assert!(inline.1.contains(r#"\"id\":7"#));

        let file = run_action(
            &["--payload", &format!("@{}", payload_file.display())],
            None,
        );
        assert!(file.0.success(), "{file:?}");
        assert!(file.1.contains("from-file"));
        assert!(!file.1.contains("do-not-echo"));

        let stdin = run_action(
            &["--payload", "-"],
            Some(r#"{"nested":{"value":"from-stdin"}}"#),
        );
        assert!(stdin.0.success(), "{stdin:?}");
        assert!(stdin.1.contains("from-stdin"));

        let invalid = run_action(&["--payload", "{"], None);
        assert!(!invalid.0.success(), "{invalid:?}");
        assert!(invalid.1.contains("action payload"));
        assert!(!invalid.1.contains("secret"));

        let failing = run_action(&["--payload", r#"{"exit":3}"#], None);
        assert_eq!(failing.0.code(), Some(3), "{failing:?}");
    });
}

fn install_fixture_extension(home: &Path) {
    let directory = home
        .join(".config/homeboy/extensions")
        .join("action-payload-fixture");
    std::fs::create_dir_all(&directory).expect("extension directory");
    std::fs::write(
        directory.join("action-payload-fixture.json"),
        r#"{
            "name":"Action payload fixture",
            "version":"1.0.0",
            "actions":[
                {"id":"capture","label":"Capture","type":"command","payload":{"received":"{{payload.nested}}","selected":"{{selected}}","exit":"{{payload.exit}}"},"command":"if printf '%s' \"$HOMEBOY_SETTINGS_JSON\" | grep -q '\\\"exit\\\":3'; then exit 3; fi; printf '%s' \"$HOMEBOY_SETTINGS_JSON\""}
            ]
        }"#,
    )
    .expect("extension manifest");
}

fn run_action(args: &[&str], stdin: Option<&str>) -> (ExitStatus, String) {
    let output_path = std::env::temp_dir().join(format!(
        "homeboy-action-payload-test-{}.json",
        std::process::id()
    ));
    let mut command = Command::new(homeboy_bin());
    command
        .args(["--output", output_path.to_str().expect("output path")])
        .args(["extension", "action", "action-payload-fixture", "capture"])
        .args(args)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1");
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().expect("run action command");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("stdin pipe")
            .write_all(input.as_bytes())
            .expect("write action payload");
    }
    let output = child.wait_with_output().expect("collect action output");
    let report = std::fs::read_to_string(&output_path).unwrap_or_else(|error| {
        panic!(
            "read structured action output {}: {error}",
            output_path.display()
        )
    });
    let _ = std::fs::remove_file(output_path);
    (output.status, report)
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
