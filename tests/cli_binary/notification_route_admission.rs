use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
#[test]
fn cook_batch_uses_cooks_ambient_route_admission_without_affecting_previews() {
    let home = tempfile::tempdir().expect("temporary home");
    let sentinel = home.path().join("resolver-invoked");
    write_notification_resolver(home.path(), &sentinel);

    let output = command(
        &home,
        &[
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "fixture",
            "https://github.com/Extra-Chill/homeboy/issues/14195",
        ],
    )
    .output()
    .expect("run cook-batch");
    assert!(
        !output.status.success(),
        "fixture repository must not reach execution"
    );
    assert!(
        sentinel.exists(),
        "cook-batch must admit the installed ambient route before command execution"
    );

    fs::remove_file(&sentinel).expect("clear resolver sentinel");
    let output = command(
        &home,
        &[
            "--notification-transport",
            "explicit.completed",
            "--notification-route",
            "opaque-explicit-route",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "fixture",
            "https://github.com/Extra-Chill/homeboy/issues/14195",
        ],
    )
    .output()
    .expect("run explicit cook-batch");
    assert!(
        !output.status.success(),
        "fixture repository must not execute"
    );
    assert!(
        !sentinel.exists(),
        "an explicit route must take precedence over ambient resolution"
    );

    let output = command(
        &home,
        &[
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "fixture",
            "https://github.com/Extra-Chill/homeboy/issues/14195",
        ],
    )
    .env("HOMEBOY_NOTIFICATION_TRANSPORT", "propagated.completed")
    .env("HOMEBOY_NOTIFICATION_ROUTE", "opaque-propagated-route")
    .output()
    .expect("run propagated cook-batch");
    assert!(
        !output.status.success(),
        "fixture repository must not execute"
    );
    assert!(
        !sentinel.exists(),
        "a propagated route must take precedence over ambient resolution"
    );

    for args in [
        &["agent-task", "cook", "--preview"][..],
        &[
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "fixture",
            "--preview",
            "https://github.com/Extra-Chill/homeboy/issues/14195",
        ][..],
    ] {
        let _ = command(&home, args).output().expect("run preview");
        assert!(
            !sentinel.exists(),
            "preview must not invoke an ambient notification resolver: {args:?}"
        );
    }
}

#[cfg(unix)]
fn write_notification_resolver(home: &Path, sentinel: &Path) {
    let extension = home.join(".config/homeboy/extensions/resolver-test");
    fs::create_dir_all(&extension).expect("extension directory");
    let resolver = format!(
        "touch '{}'; printf '%s' '{{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"matched\",\"route\":\"opaque-route\"}}'",
        sentinel.display()
    );
    fs::write(
        extension.join("resolver-test.json"),
        serde_json::json!({
            "name": "Notification resolver fixture",
            "version": "0.0.0",
            "notification_transports": [{
                "id": "test.completed",
                "command": ["true"],
                "route_resolver": { "command": ["sh", "-c", resolver] }
            }]
        })
        .to_string(),
    )
    .expect("extension manifest");
}

#[cfg(unix)]
fn command(home: &tempfile::TempDir, args: &[&str]) -> Command {
    let mut command = Command::new(homeboy_bin());
    command
        .args(args)
        .env_clear()
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
