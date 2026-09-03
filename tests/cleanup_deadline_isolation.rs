#![cfg(unix)]

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn hanging_category_returns_typed_partial_evidence_and_later_category_completes() {
    let fixture = CleanupHangFixture::new();
    let started = Instant::now();
    let output = fixture.run(&["cleanup", "--include", "repo-artifacts,controller-runtimes"]);

    assert!(started.elapsed() < Duration::from_secs(15), "{output:#}");
    assert_eq!(output["success"], false, "{output:#}");
    assert_eq!(output["data"]["status"], "partial_failure", "{output:#}");
    assert_eq!(output["data"]["failed_category_count"], 0, "{output:#}");
    let categories = output["data"]["categories"]
        .as_array()
        .expect("cleanup categories");
    assert_eq!(categories.len(), 2, "{output:#}");
    assert_eq!(categories[0]["category"], "repo_artifacts");
    assert_eq!(categories[0]["outcome"], "timed_out");
    assert_eq!(categories[0]["failure"]["code"], "cleanup.category_timeout");
    assert_eq!(categories[0]["inventory_completeness"], "partial");
    assert_eq!(
        categories[0]["continuation_command"],
        "homeboy cleanup --include repo-artifacts"
    );
    assert!(categories[0]["elapsed_ms"].as_u64().unwrap_or(0) >= 100);
    assert!(categories[0]["last_progress"]
        .as_str()
        .is_some_and(|progress| progress.contains("repo_artifacts")));
    assert_eq!(categories[1]["category"], "controller_runtimes");
    assert_eq!(categories[1]["outcome"], "completed", "{output:#}");
    assert!(categories[1]["failure"].is_null(), "{output:#}");

    fixture.release_descendant();
    assert!(
        !fixture.survivor.exists(),
        "the timed-out category's descendant survived its process-group deadline"
    );
}

#[test]
fn excluded_worktree_categories_never_enter_their_internals() {
    let fixture = CleanupHangFixture::new();
    let output = fixture.run(&[
        "cleanup",
        "--include",
        "repo-artifacts,task-worktrees",
        "--exclude",
        "task-worktrees",
    ]);

    assert_eq!(output["data"]["category_count"], 1, "{output:#}");
    let invoked = std::fs::read_to_string(&fixture.invoked).expect("fixture invocation log");
    assert_eq!(invoked.lines().collect::<Vec<_>>(), ["repo_artifacts"]);
    assert!(!invoked.contains("task_worktrees"));
}

#[test]
fn retained_storage_hang_returns_a_bounded_typed_continuation() {
    let fixture = CleanupHangFixture::new();
    let started = Instant::now();
    let output = fixture.run(&["cleanup", "retained-storage", "--limit", "7"]);

    assert!(started.elapsed() < Duration::from_secs(15), "{output:#}");
    assert_eq!(output["success"], false, "{output:#}");
    assert_eq!(output["data"]["status"], "partial_failure", "{output:#}");
    assert_eq!(output["data"]["outcome"], "timed_out", "{output:#}");
    assert_eq!(
        output["data"]["failure"]["code"],
        "cleanup.category_timeout"
    );
    assert_eq!(
        output["data"]["continuation_command"],
        "homeboy cleanup retained-storage --limit 7"
    );
    assert!(output["data"]["last_progress"]
        .as_str()
        .is_some_and(|progress| progress.contains("retained_storage")));

    fixture.release_descendant();
    assert!(!fixture.survivor.exists());
}

struct CleanupHangFixture {
    root: tempfile::TempDir,
    script: PathBuf,
    invoked: PathBuf,
    release: PathBuf,
    survivor: PathBuf,
}

impl CleanupHangFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("cleanup fixture");
        let script = root.path().join("cleanup-category-fixture.sh");
        let invoked = root.path().join("invoked");
        let release = root.path().join("release");
        let survivor = root.path().join("survivor");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$1\" >> '{}'\ncase \"$1\" in\n  repo_artifacts|retained_storage)\n    (while [ ! -f '{}' ]; do sleep 0.01; done; touch '{}') &\n    while :; do sleep 1; done\n    ;;\nesac\n",
                invoked.display(),
                release.display(),
                survivor.display(),
            ),
        )
        .expect("write cleanup fixture");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make cleanup fixture executable");
        Self {
            root,
            script,
            invoked,
            release,
            survivor,
        }
    }

    fn run(&self, args: &[&str]) -> Value {
        self.run_with_timeout(args, 12_000)
    }

    fn run_with_timeout(&self, args: &[&str], timeout_ms: u64) -> Value {
        let output = Command::new(homeboy_bin())
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .env("HOMEBOY_TEST_CLEANUP_CATEGORY_FIXTURE", &self.script)
            .env(
                "HOMEBOY_TEST_CLEANUP_CATEGORY_TIMEOUT_MS",
                timeout_ms.to_string(),
            )
            .env_remove("HOMEBOY_INTERNAL_CLEANUP_CATEGORY_CHILD")
            .output()
            .expect("run cleanup fixture");
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "cleanup output was not JSON ({error}); status={:?}; stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn release_descendant(&self) {
        std::fs::write(&self.release, "release").expect("release fixture descendant");
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
