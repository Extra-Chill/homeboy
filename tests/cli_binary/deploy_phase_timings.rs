use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An exact-ref deploy to a local target reports ordered per-component phase
/// timings, and `HOMEBOY_STATUS=1` surfaces `phase=` lines on redirected stderr.
#[test]
fn exact_ref_deploy_reports_ordered_phase_timings_and_status_lines() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let source = temp.path().join("source");
    let site = temp.path().join("site");
    for dir in [&home, &source, &site.join("plugins")] {
        std::fs::create_dir_all(dir).expect("fixture dir");
    }

    std::fs::write(source.join("fixture.php"), "<?php\n/* Version: 1.0.0 */\n").expect("source");
    std::fs::write(source.join(".gitignore"), "build/\n").expect("gitignore");
    std::fs::write(
        source.join("homeboy.json"),
        serde_json::json!({
            "id": "fixture",
            "remote_path": "plugins/fixture",
            "build_artifact": "build/fixture.php",
            "version_targets": [{ "file": "fixture.php", "pattern": "Version:\\s*([0-9.]+)" }],
            "scripts": { "build": ["mkdir -p build && cp fixture.php build/fixture.php"] },
        })
        .to_string(),
    )
    .expect("component manifest");
    git(&source, &["init", "-q"]);
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-qm", "fixture"]);
    let sha = String::from_utf8(git(&source, &["rev-parse", "HEAD"]).stdout)
        .expect("sha")
        .trim()
        .to_string();

    let site_arg = site.to_str().expect("site path");
    let source_arg = source.to_str().expect("source path");
    homeboy(
        &home,
        &[
            "server",
            "create",
            "local",
            "--host",
            "localhost",
            "--user",
            "test",
        ],
    );
    homeboy(
        &home,
        &[
            "project",
            "create",
            "site",
            "--server-id",
            "local",
            "--base-path",
            site_arg,
        ],
    );
    homeboy(
        &home,
        &["project", "components", "attach-path", "site", source_arg],
    );

    let out = temp.path().join("out.json");
    let deploy = homeboy(
        &home,
        &[
            "deploy",
            "site",
            "fixture",
            "--ref",
            &sha,
            "--confirm-dangerous",
            "--output",
            out.to_str().expect("out path"),
        ],
    );
    let stderr = String::from_utf8_lossy(&deploy.stderr);
    let output: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).expect("--output file")).expect("JSON output");
    let result = &output["data"]["results"][0];
    assert_eq!(result["status"], "deployed", "{output}\n{stderr}");

    let spans = result["phase_timings"]["spans"]
        .as_array()
        .unwrap_or_else(|| panic!("phase_timings missing: {result}"));
    let ids: Vec<&str> = spans
        .iter()
        .map(|span| span["id"].as_str().expect("span id"))
        .collect();
    assert_eq!(
        ids,
        [
            "source.materialize",
            "source.verify",
            "source.dependencies",
            "source.package_inputs",
            "build",
            "transfer",
            "verify.version",
            "hooks",
            "project.hooks",
            "project.smoke",
        ]
    );
    let starts: Vec<&str> = spans
        .iter()
        .map(|span| span["started_at"].as_str().expect("started_at"))
        .collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] <= pair[1]),
        "{starts:?}"
    );
    assert!(spans.iter().all(|span| span["duration_ms"].is_number()));

    for id in ids {
        assert!(
            stderr.contains(&format!("phase={id} ")),
            "missing phase={id} status line in redirected stderr:\n{stderr}"
        );
    }
}

fn homeboy(home: &Path, args: &[&str]) -> Output {
    let output = Command::new(homeboy_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOMEBOY_DATA_DIR")
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_STATUS", "1")
        .output()
        .expect("run homeboy");
    assert!(
        output.status.success(),
        "homeboy {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed");
    output
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
