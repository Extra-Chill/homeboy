#![cfg(test)]

mod dispatch;
mod handoff;

use super::*;
use clap::Parser;
use homeboy::command_contract::{lab_runner_supports_contract_label, LabCommandPortability};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::tempdir;

use super::*;

pub(super) fn git_init(path: &Path) {
    let output = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output()
        .expect("initialize git workspace");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
    _guard: MutexGuard<'static, ()>,
}

pub(super) struct CwdGuard {
    previous: std::path::PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(name: &'static str, value: &str) -> Self {
        Self::set_many(&[(name, Some(value))])
    }

    fn remove(name: &'static str) -> Self {
        Self::set_many(&[(name, None)])
    }

    fn set_many(changes: &[(&'static str, Option<&str>)]) -> Self {
        let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let mut previous = Vec::with_capacity(changes.len());
        for (name, value) in changes {
            previous.push((*name, std::env::var(name).ok()));
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        Self {
            previous,
            _guard: guard,
        }
    }
}

pub(super) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, previous) in self.previous.iter().rev() {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

impl CwdGuard {
    fn set(path: &std::path::Path) -> Self {
        let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).expect("restore current dir");
    }
}

pub(super) fn write_rig_source_metadata(home: &Path, rig_id: &str, linked: bool) {
    let sources_dir = home.join(".config").join("homeboy").join("rig-sources");
    fs::create_dir_all(&sources_dir).expect("create rig sources dir");
    let metadata = serde_json::json!({
        "source": "/tmp/rig-package",
        "source_root": "/tmp/rig-package",
        "package_path": "/tmp/rig-package",
        "rig_path": format!("/tmp/rig-package/rigs/{rig_id}/rig.json"),
        "discovery_path": "/tmp/rig-package",
        "linked": linked,
        "materialized": false
    });
    fs::write(
        sources_dir.join(format!("{rig_id}.json")),
        serde_json::to_string_pretty(&metadata).expect("serialize rig source metadata"),
    )
    .expect("write rig source metadata");
}

pub(super) fn write_command_only_rig(home: &Path, rig_id: &str) {
    let rigs_dir = home.join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rigs_dir).expect("create rigs dir");
    let spec = serde_json::json!({
        "id": rig_id,
        "description": "command-only rig",
        "pipeline": {
            "up": [
                {
                    "kind": "command",
                    "command": "./scripts/run-matrix.sh",
                    "cwd": "tools",
                    "env": { "MATRIX": "portable" },
                    "label": "run matrix"
                }
            ]
        }
    });
    fs::write(
        rigs_dir.join(format!("{rig_id}.json")),
        serde_json::to_string_pretty(&spec).expect("serialize rig"),
    )
    .expect("write rig");
}
