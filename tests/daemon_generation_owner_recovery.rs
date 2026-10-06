//! Admission recovery follows the selected generation, not a draining root store.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use homeboy_core::api_jobs::{JobStatus, JobStore};
use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};
use serde_json::{json, Value};
use std::os::fd::AsRawFd;

struct GenerationFixture {
    context: HermeticTestContext,
    children: Vec<Child>,
    root_state: Value,
    selected_state: Value,
    selected_dir: PathBuf,
    root_job: uuid::Uuid,
}

impl GenerationFixture {
    fn new() -> Self {
        let context = HermeticTestContext::new();
        let mut children = Vec::new();
        let root_state = launch(&context, &context.daemon_dir(), &mut children);
        let selected_dir = context.daemon_dir().join("generations/selected");
        let selected_state = launch(&context, &selected_dir, &mut children);
        let jobs =
            JobStore::open_without_reconciliation(context.daemon_dir().join("jobs.json")).unwrap();
        let root_job = jobs.create("protected-root-work");
        jobs.start(root_job.id).unwrap();

        // Seed the exact router contract from the two real published leases.
        // The root has active work and remains draining while B owns admission.
        let endpoint = |state: &Value, dir: &Path| {
            json!({
                "endpoint": {
                    "lease_id": state["lease_id"], "address": state["address"],
                    "state_dir": dir, "build_identity": state["build_identity"]["display"]
                },
                "active_jobs": 0, "drain_state": "admitting"
            })
        };
        let root_lease = root_state["lease_id"].as_str().unwrap();
        let selected_lease = selected_state["lease_id"].as_str().unwrap();
        let mut root = endpoint(&root_state, &context.daemon_dir());
        root["active_jobs"] = json!(1);
        root["drain_state"] = json!("draining");
        fs::write(
            context.daemon_dir().join("generations.json"),
            serde_json::to_vec(&json!({
                "schema": "homeboy.daemon.generations.v1",
                "generations": {
                    "admission_owner": selected_lease,
                    "generations": {
                        root_lease: root,
                        selected_lease: endpoint(&selected_state, &selected_dir)
                    },
                    "job_owners": { root_job.id.to_string(): root_lease }
                },
                "completed_jobs": []
            }))
            .unwrap(),
        )
        .unwrap();
        children[1].kill().unwrap();
        children[1].wait().unwrap();
        Self {
            context,
            children,
            root_state,
            selected_state,
            selected_dir,
            root_job: root_job.id,
        }
    }

    fn cli(&self, args: &[&str]) -> Value {
        let mut command = self.context.command(TestBinary::HomeboyFixture);
        command
            .env_remove("HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP")
            .env("HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS", "0")
            .args(args);
        let output = bounded_output(command);
        assert!(
            output.status.success(),
            "{args:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
    }

    fn assert_root_preserved(&self) {
        assert!(homeboy_core::process::pid_is_running(
            self.root_state["pid"].as_u64().unwrap() as u32
        ));
        let root: Value = serde_json::from_slice(
            &fs::read(self.context.daemon_dir().join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(root["lease_id"], self.root_state["lease_id"]);
        assert_eq!(root["startup_token"], self.root_state["startup_token"]);
        let jobs =
            JobStore::open_without_reconciliation(self.context.daemon_dir().join("jobs.json"))
                .unwrap();
        assert_eq!(jobs.get(self.root_job).unwrap().status, JobStatus::Running);
    }

    fn assert_successor(&self, result: &Value) {
        assert_ne!(result["lease_id"], self.root_state["lease_id"]);
        assert_ne!(result["lease_id"], self.selected_state["lease_id"]);
        assert!(Path::new(result["state_path"].as_str().unwrap())
            .starts_with(self.context.daemon_dir().join("generations")));
        let status = self.cli(&["daemon", "status"]);
        assert_eq!(status["fresh"], true);
        assert_eq!(status["daemon"]["lease_id"], result["lease_id"]);
        let replay = self.cli(&["daemon", "ensure-running"]);
        assert_eq!(replay["lease_id"], result["lease_id"]);
        self.assert_root_preserved();
    }
}

impl Drop for GenerationFixture {
    fn drop(&mut self) {
        // Retire only fixture-owned published generations. Original foreground
        // children are reaped by their exact Child handles, even on assertion failure.
        let registry = self.context.daemon_dir().join("generations.json");
        if let Ok(bytes) = fs::read(registry) {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(entries) = value["generations"]["generations"].as_object() {
                    for entry in entries.values() {
                        let Some(dir) = entry["endpoint"]["state_dir"].as_str() else {
                            continue;
                        };
                        let path = Path::new(dir).join("state.json");
                        let Ok(bytes) = fs::read(path) else { continue };
                        let Ok(state) = serde_json::from_slice::<Value>(&bytes) else {
                            continue;
                        };
                        let Some(pid) = state["pid"].as_u64() else {
                            continue;
                        };
                        if !self
                            .children
                            .iter()
                            .any(|child| u64::from(child.id()) == pid)
                        {
                            let mut stop = self.context.command(TestBinary::HomeboyFixture);
                            stop.env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, dir)
                                .env("HOMEBOY_DAEMON_ROUTER_BYPASS", "1")
                                .args(["daemon", "stop"]);
                            let _ = bounded_output(stop);
                        }
                    }
                }
            }
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn launch(context: &HermeticTestContext, dir: &Path, children: &mut Vec<Child>) -> Value {
    let token = uuid::Uuid::new_v4().to_string();
    let mut command: Command = context.command(TestBinary::HomeboyFixture);
    command
        .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, dir)
        .env("HOMEBOY_DAEMON_STARTUP_TOKEN", &token)
        .args([
            "daemon",
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--startup-token",
            &token,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    children.push(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(bytes) = fs::read(dir.join("state.json")) {
            if let Ok(state) = serde_json::from_slice::<Value>(&bytes) {
                return state;
            }
        }
        assert!(
            Instant::now() < deadline,
            "foreground fixture publishes lease"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn ensure_replaces_dead_admission_without_touching_live_root_work() {
    let fixture = GenerationFixture::new();
    let result = fixture.cli(&["daemon", "ensure-running"]);
    fixture.assert_successor(&result);
}

#[test]
fn start_replaces_missing_admission_without_using_root_owner_lock() {
    let fixture = GenerationFixture::new();
    fs::remove_file(fixture.selected_dir.join("state.json")).unwrap();
    let result = fixture.cli(&["daemon", "start"]);
    fixture.assert_successor(&result);
}

#[test]
fn native_recovery_restores_admission_without_touching_live_root_work() {
    let fixture = GenerationFixture::new();
    let result = fixture.cli(&["daemon", "recover", "--yes"]);
    assert_eq!(result["fresh"], true);
    let status = fixture.cli(&["daemon", "status"]);
    assert_eq!(status["daemon"]["lease_id"], result["lease_id"]);
    assert_ne!(status["daemon"]["lease_id"], fixture.selected_state["lease_id"]);
    fixture.assert_root_preserved();
}

#[test]
fn missing_selected_lease_with_unverified_owner_refuses_replacement() {
    let fixture = GenerationFixture::new();
    fs::remove_file(fixture.selected_dir.join("state.json")).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.selected_dir.join("owner.lock"))
        .unwrap();
    // SAFETY: this live File owns the descriptor throughout the lock fixture.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let before = fs::read(fixture.context.daemon_dir().join("generations.json")).unwrap();
    let status = fixture.cli(&["daemon", "status"]);
    assert_eq!(status["recovery"]["restartable"], false);
    assert_eq!(status["recovery"]["replacement_blocked"], true);
    let mut command = fixture.context.command(TestBinary::HomeboyFixture);
    command.args(["daemon", "ensure-running"]);
    let result = bounded_output(command);
    assert!(!result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        value["diagnostics"]["details"]["classification"],
        "daemon_generation_owner_unverified"
    );
    assert_eq!(
        fs::read(fixture.context.daemon_dir().join("generations.json")).unwrap(),
        before
    );
    fixture.assert_root_preserved();
}
