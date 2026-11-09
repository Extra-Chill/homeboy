#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

/// Exercise the public command (also the workflow homeboy-step entry point),
/// including every hold and GitHub's atomic head guard.
#[test]
fn exact_head_merge_gate_public_command() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    fs::write(&gh, r#"#!/bin/sh
case "$1 $2" in
  'pr view') printf '%s' "$VIEW" ;;
  'api repos/acme/project') printf '{"allow_squash_merge":false,"allow_merge_commit":false,"allow_rebase_merge":true}' ;;
  'api repos/acme/project/pulls/7/merge')
    printf '%s\n' "$*" > "$MERGE_LOG"
    if [ "$RACE" = yes ]; then echo 'HTTP 409' >&2; exit 1; fi
    printf '{"merged":true}' ;;
  *) exit 98 ;;
esac
"#).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let sha = "a".repeat(40);
    let base = serde_json::json!({"headRefOid":sha,"mergeable":"MERGEABLE","isDraft":false,"state":"OPEN",
        "statusCheckRollup":[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"}, {"context":"legacy","state":"SUCCESS"}]});
    for reason in [
        "merged",
        "head_moved",
        "checks_failed",
        "checks_missing",
        "conflicting",
        "timeout",
        "race",
    ] {
        let mut view = base.clone();
        match reason {
            "head_moved" => view["headRefOid"] = "b".repeat(40).into(),
            "checks_failed" => view["statusCheckRollup"][0]["conclusion"] = "FAILURE".into(),
            "checks_missing" => view["statusCheckRollup"] = serde_json::json!([]),
            "conflicting" => view["mergeable"] = "CONFLICTING".into(),
            "timeout" => view["mergeable"] = "UNKNOWN".into(),
            _ => {}
        }
        let log = dir.path().join("merge.log");
        let _ = fs::remove_file(&log);
        let output = Command::new(env!("CARGO_BIN_EXE_homeboy"))
            .args([
                "git",
                "pr",
                "merge-when-green",
                "--repo",
                "acme/project",
                "--number",
                "7",
                "--sha",
                &sha,
                "--wait-seconds",
                "1",
            ])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("VIEW", view.to_string())
            .env("MERGE_LOG", &log)
            .env("RACE", if reason == "race" { "yes" } else { "no" })
            .output()
            .unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        let data = &envelope["data"];
        assert_eq!(output.status.success(), reason == "merged", "{envelope}");
        assert_eq!(data["merged"], reason == "merged", "{envelope}");
        if reason != "merged" {
            assert_eq!(
                data["hold"]["reason"],
                if reason == "race" {
                    "head_moved"
                } else {
                    reason
                },
                "{envelope}"
            );
        }
        if reason == "checks_failed" {
            assert_eq!(data["hold"]["names"], serde_json::json!(["ci"]));
        }
        if reason == "merged" || reason == "race" {
            let args = fs::read_to_string(&log).unwrap();
            assert!(args.contains(&format!("sha={sha}")));
            assert!(args.contains("merge_method=rebase"));
        } else {
            assert!(!log.exists());
        }
    }
}
