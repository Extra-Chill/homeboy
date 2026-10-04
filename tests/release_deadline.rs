use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use std::path::PathBuf;

fn version_fixture(context: &HermeticTestContext) -> PathBuf {
    let root = context.root().join("version-fixture");
    std::fs::create_dir(&root).expect("create version fixture");
    std::fs::write(
        root.join("homeboy.json"),
        r#"{
            "id": "version-fixture",
            "version_targets": [{"file": "VERSION", "pattern": "([0-9]+\\.[0-9]+\\.[0-9]+)"}]
        }"#,
    )
    .expect("write component config");
    root
}

#[test]
fn release_version_show_reads_the_fixture_without_timing_out() {
    let context = HermeticTestContext::new();
    let root = version_fixture(&context);
    std::fs::write(root.join("VERSION"), "4.5.6\n").expect("write version");
    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(["release", "version", "show", "version-fixture", "--path"])
        .arg(&root)
        .env_remove("HOMEBOY_RELEASE_DEADLINE_CHILD")
        .env_remove("HOMEBOY_RELEASE_DEADLINE_SECS")
        .output()
        .expect("run release version show");

    assert!(output.status.success(), "{output:?}");
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("version JSON envelope");
    assert_eq!(envelope["data"]["version"], "4.5.6");
}

/// A FIFO with no writer cannot finish the version read. The outcome depends
/// on the watchdog's kill/envelope path, rather than normal work racing the
/// host's speed (#14880). Nextest supplies the outer hang-containment budget.
#[cfg(unix)]
#[test]
fn release_version_show_emits_a_structured_timeout_for_a_blocked_read() {
    use std::os::unix::ffi::OsStrExt;

    let context = HermeticTestContext::new();
    let root = version_fixture(&context);
    let fifo = std::ffi::CString::new(root.join("VERSION").as_os_str().as_bytes())
        .expect("FIFO path has no NUL");
    assert_eq!(
        unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) },
        0,
        "create blocking version fixture: {}",
        std::io::Error::last_os_error()
    );
    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(["release", "version", "show", "version-fixture", "--path"])
        .arg(&root)
        .env_remove("HOMEBOY_RELEASE_DEADLINE_CHILD")
        .env("HOMEBOY_RELEASE_DEADLINE_SECS", "2")
        .output()
        .expect("run blocked release version show");

    assert_eq!(output.status.code(), Some(124), "{output:?}");
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("timeout must emit one JSON envelope");
    assert_eq!(envelope["schema"], "homeboy/command-result/v3");
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["exit_code"], 124);
    assert!(envelope["diagnostics"]["details"]["error"]
        .as_str()
        .expect("timeout cause")
        .contains("version show: resolving component"));
}
