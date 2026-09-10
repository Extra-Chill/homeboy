use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

#[test]
fn dev_build_accepts_matching_full_identity_and_rejects_mismatch() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let tools = fixture.path().join("tools");
    let target = fixture.path().join("target");
    fs::create_dir(&tools).expect("fixture tools directory");

    write_executable(
        &tools.join("cargo"),
        r#"#!/usr/bin/env bash
set -euo pipefail
binary="$CARGO_TARGET_DIR/debug/homeboy"
mkdir -p "$(dirname "$binary")"
printf '%s\n' '#!/usr/bin/env bash' 'printf "{\\"data\\":{\\"git_commit\\":\\"%s\\"}}\\n" "$HOMEBOY_TEST_BINARY_COMMIT"' > "$binary"
chmod +x "$binary"
"#,
    );
    write_executable(
        &tools.join("jq"),
        "#!/usr/bin/env bash\ncat >/dev/null\nprintf '%s\\n' \"$HOMEBOY_TEST_BINARY_COMMIT\"\n",
    );

    let expected = git_head();
    let matching = run_dev_build(&tools, &target, &expected);
    assert!(
        matching.status.success(),
        "matching full identity failed with {}:\nstdout: {}\nstderr: {}",
        matching.status,
        String::from_utf8_lossy(&matching.stdout),
        String::from_utf8_lossy(&matching.stderr)
    );
    assert!(String::from_utf8_lossy(&matching.stdout).contains(&expected));

    let mismatch = run_dev_build(&tools, &target, "different-commit");
    assert!(!mismatch.status.success());
    assert!(
        String::from_utf8_lossy(&mismatch.stderr).contains(&format!(
            "development binary identity mismatch: expected {expected}, got different-commit"
        )),
        "mismatched identity did not report the expected failure:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&mismatch.stdout),
        String::from_utf8_lossy(&mismatch.stderr)
    );
}

fn run_dev_build(tools: &Path, target: &Path, binary_commit: &str) -> std::process::Output {
    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").expect("PATH")
    );
    Command::new(env!("CARGO_MANIFEST_DIR").to_owned() + "/scripts/dev-build")
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("HOMEBOY_TEST_BINARY_COMMIT", binary_commit)
        .output()
        .expect("run dev-build")
}

fn git_head() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("resolve fixture HEAD");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("HEAD is UTF-8")
        .trim()
        .to_string()
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).expect("write fixture executable");
    let mut permissions = fs::metadata(path)
        .expect("fixture executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fixture executable");
}
