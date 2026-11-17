use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

/// The doctor looks for a Playwright CLI on PATH even when no tool registry
/// entry declares one, instead of reporting it missing without looking.
#[test]
#[cfg(unix)]
fn local_doctor_finds_playwright_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().expect("home");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let playwright = bin.join("playwright");
    std::fs::write(&playwright, "#!/bin/sh\necho 'Version 1.0.0'\n").expect("fake playwright");
    std::fs::set_permissions(&playwright, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::create_dir_all(home.path().join(".cache/ms-playwright/chromium-1"))
        .expect("browser cache");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let homeboy = |args: &[&str]| {
        Command::new(PathBuf::from(
            std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"),
        ))
        .args(args)
        .env("HOME", home.path())
        .env("PATH", &path)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy")
    };
    let workspace = home.path().display().to_string();
    let added = homeboy(&[
        "runner",
        "add",
        "lab-local",
        "--kind",
        "local",
        "--workspace-root",
        &workspace,
    ]);
    assert!(added.status.success(), "{added:?}");

    let doctor = homeboy(&["runner", "doctor", "lab-local", "--full"]);
    let report: Value = serde_json::from_slice(&doctor.stdout).expect("doctor json");
    let check = report["data"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["id"] == "playwright.browser_ready")
        .expect("playwright check")
        .clone();
    assert_eq!(check["status"], "ok", "{check:#}");
}
