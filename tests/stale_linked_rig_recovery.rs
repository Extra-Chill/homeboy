use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn run_homeboy(home: &Path, args: &[&str]) -> Output {
    Command::new(homeboy_bin())
        .args(["--placement", "local"])
        .args(args)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy")
}

fn assert_success(output: Output, args: &[&str]) {
    assert!(
        output.status.success(),
        "homeboy {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_bench_extension(home: &Path) {
    let extension_dir = home.join(".config/homeboy/extensions/fixture-bench");
    fs::create_dir_all(&extension_dir).expect("create extension directory");
    fs::write(
        extension_dir.join("fixture-bench.json"),
        r#"{
            "name": "Fixture Bench",
            "version": "0.0.0",
            "bench": { "extension_script": "bench-runner.sh" }
        }"#,
    )
    .expect("write extension manifest");
    let script = extension_dir.join("bench-runner.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","iterations":${HOMEBOY_BENCH_ITERATIONS:-0},"scenarios":[{"id":"recovery","iterations":${HOMEBOY_BENCH_ITERATIONS:-0},"metrics":{"p95_ms":1.0}}],"metric_policies":{"p95_ms":{"direction":"lower_is_better"}}}
JSON
"#,
    )
    .expect("write bench runner");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make bench runner executable");
    }
}

fn write_package_rig(package: &Path, id: &str) {
    let rig_dir = package.join("rigs").join(id);
    fs::create_dir_all(&rig_dir).expect("create rig package");
    fs::write(
        rig_dir.join("rig.json"),
        format!(
            r#"{{
                "id": "{id}",
                "components": {{
                    "studio": {{
                        "path": "{}",
                        "extensions": {{ "fixture-bench": {{}} }}
                    }}
                }},
                "bench": {{ "default_component": "studio" }}
            }}"#,
            package.display()
        ),
    )
    .expect("write rig package");
}

fn install_then_remove_source_a(home: &Path, source_a: &Path) {
    let source_a_text = source_a.to_str().expect("source A path");
    assert_success(
        run_homeboy(home, &["rig", "install", source_a_text]),
        &["rig", "install", source_a_text],
    );
    fs::remove_dir_all(source_a).expect("remove source A");
}

#[test]
fn bench_repairs_missing_linked_rig_source_from_path_override() {
    let home = tempfile::tempdir().expect("home");
    let source_a = tempfile::tempdir().expect("source A");
    let checkout_b = tempfile::tempdir().expect("checkout B");
    write_bench_extension(home.path());
    write_package_rig(source_a.path(), "stale-rig");
    write_package_rig(checkout_b.path(), "stale-rig");
    install_then_remove_source_a(home.path(), source_a.path());

    let checkout_b_text = checkout_b.path().to_str().expect("checkout B path");
    let args = [
        "bench",
        "--rig",
        "stale-rig",
        "--path",
        checkout_b_text,
        "--iterations",
        "1",
    ];
    assert_success(run_homeboy(home.path(), &args), &args);

    let metadata = fs::read_to_string(
        home.path()
            .join(".config/homeboy/rig-sources/stale-rig.json"),
    )
    .expect("refreshed source metadata");
    let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata JSON");
    let expected_package_path = checkout_b.path().to_string_lossy().into_owned();
    assert_eq!(
        metadata["package_path"].as_str(),
        Some(expected_package_path.as_str())
    );
}

#[test]
fn bench_recovery_preserves_conflicting_user_owned_rig() {
    let home = tempfile::tempdir().expect("home");
    let source_a = tempfile::tempdir().expect("source A");
    let checkout_b = tempfile::tempdir().expect("checkout B");
    write_bench_extension(home.path());
    write_package_rig(source_a.path(), "stale-rig");
    write_package_rig(checkout_b.path(), "stale-rig");
    install_then_remove_source_a(home.path(), source_a.path());

    let config_path = home.path().join(".config/homeboy/rigs/stale-rig.json");
    fs::remove_file(&config_path).expect("remove stale rig link");
    let user_owned = r#"{ "id": "other-rig", "components": {} }"#;
    fs::write(&config_path, user_owned).expect("write conflicting user rig");

    let checkout_b_text = checkout_b.path().to_str().expect("checkout B path");
    let output = run_homeboy(
        home.path(),
        &[
            "bench",
            "--rig",
            "stale-rig",
            "--path",
            checkout_b_text,
            "--iterations",
            "1",
        ],
    );

    assert!(!output.status.success(), "conflicting rig must fail");
    assert!(String::from_utf8_lossy(&output.stdout).contains("refusing to replace"));
    assert_eq!(
        fs::read_to_string(config_path).expect("read user rig"),
        user_owned
    );
}
