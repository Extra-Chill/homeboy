//! Controller-operated retention for producer-owned runner Homeboy binaries.

use serde::Serialize;
use std::process::Command;
use std::time::Duration;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};

use crate::{load, workspace::ssh_client_for_runner, Runner, RunnerKind};

const CACHE_PRUNE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct RunnerBinaryCachePruneOptions {
    pub apply: bool,
    pub min_age_hours: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerBinaryCachePruneEntry {
    pub path: String,
    pub bytes: u64,
    pub age_seconds: u64,
    pub identity: String,
    pub state: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerBinaryCachePruneOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub cache_root: String,
    pub entries: Vec<RunnerBinaryCachePruneEntry>,
    pub eligible: Vec<RunnerBinaryCachePruneEntry>,
    pub removed: Vec<RunnerBinaryCachePruneEntry>,
    pub skipped: Vec<RunnerBinaryCachePruneEntry>,
    pub total_bytes: u64,
    pub eligible_bytes: u64,
    pub removed_bytes: u64,
}

/// Inventory and prune managed slots without relying on a runner daemon. The
/// remote shell is deliberately the authority for process ownership and the
/// destructive revalidation, so SSH runners work while disconnected.
pub fn prune_homeboy_binary_cache(
    runner_id: &str,
    options: RunnerBinaryCachePruneOptions,
) -> Result<(RunnerBinaryCachePruneOutput, i32)> {
    let _promotion = options
        .apply
        .then(|| {
            homeboy_core::runtime_promotion::acquire(
                "runner binary cache prune",
                runner_id.to_string(),
            )
        })
        .transpose()?;
    let runner = load(runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner cache-prune requires workspace_root",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let root = format!("{}/_homeboy_binaries", workspace_root.trim_end_matches('/'));
    let configured = runner.settings.homeboy_path.as_deref().unwrap_or("");
    let command = inventory_command(
        &root,
        configured,
        options.min_age_hours.saturating_mul(3600),
    );
    let scan = execute_direct(&runner, &command)?;
    if !scan.success {
        return Err(Error::internal_unexpected(format!(
            "runner binary cache inventory failed: {}",
            scan.stderr.trim()
        )));
    }
    let entries = parse_entries(&scan.stdout)?;
    let total_bytes = entries.iter().map(|entry| entry.bytes).sum();
    let eligible = entries
        .iter()
        .filter(|entry| entry.state == "eligible")
        .cloned()
        .collect::<Vec<_>>();
    let eligible_bytes = eligible.iter().map(|entry| entry.bytes).sum();
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    if options.apply {
        for entry in &eligible {
            let command = delete_command(&root, &entry.path, &entry.identity, configured);
            let result = execute_direct(&runner, &command)?;
            let state = result.stdout.trim();
            if result.success && state == "removed" {
                removed.push(entry.clone());
            } else {
                let mut entry = entry.clone();
                entry.state = "skipped".to_string();
                entry.reason = if state.is_empty() {
                    "delete_failed".to_string()
                } else {
                    state.to_string()
                };
                skipped.push(entry);
            }
        }
    }
    let removed_bytes = removed.iter().map(|entry| entry.bytes).sum();
    Ok((
        RunnerBinaryCachePruneOutput {
            variant: "runner_binary_cache_prune",
            command: "runner.cache_prune",
            runner_id: runner.id,
            dry_run: !options.apply,
            cache_root: root,
            entries,
            eligible,
            removed,
            skipped,
            total_bytes,
            eligible_bytes,
            removed_bytes,
        },
        0,
    ))
}

fn parse_entries(stdout: &str) -> Result<Vec<RunnerBinaryCachePruneEntry>> {
    stdout
        .lines()
        .map(|line| {
            let fields = line.splitn(6, '\t').collect::<Vec<_>>();
            if fields.len() != 6 {
                return Err(Error::internal_unexpected(
                    "runner binary cache inventory returned an invalid row".to_string(),
                ));
            }
            Ok(RunnerBinaryCachePruneEntry {
                path: fields[0].to_string(),
                bytes: fields[1].parse().map_err(|error| {
                    Error::internal_unexpected(format!(
                        "runner binary cache inventory returned invalid bytes: {error}"
                    ))
                })?,
                age_seconds: fields[2].parse().map_err(|error| {
                    Error::internal_unexpected(format!(
                        "runner binary cache inventory returned invalid age: {error}"
                    ))
                })?,
                identity: fields[3].to_string(),
                state: fields[4].to_string(),
                reason: fields[5].to_string(),
            })
        })
        .collect()
}

struct DirectOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

fn execute_direct(runner: &Runner, command: &str) -> Result<DirectOutput> {
    match runner.kind {
        RunnerKind::Local => {
            let output = Command::new("sh")
                .args(["-c", command])
                .output()
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some("run local binary cache lifecycle command".to_string()),
                    )
                })?;
            Ok(DirectOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            let output = client.execute_with_timeout(command, CACHE_PRUNE_TIMEOUT);
            Ok(DirectOutput {
                success: output.success,
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }
}

fn inventory_command(root: &str, configured: &str, min_age: u64) -> String {
    // Slot names are created by refresh (`homeboy-<sanitized-ref>`) and dev-sync
    // (`dev/<sha-prefix>`). Everything else remains visible but fail-closed.
    format!(
        "root={root}; configured={configured}; min_age={min_age}; now=$(date +%s); [ -d \"$root\" ] || exit 0; {{ find \"$root\" -mindepth 1 -maxdepth 1 \\( -type d -o -type l \\) ! -name dev -print; if [ -d \"$root/dev\" ] && [ ! -L \"$root/dev\" ]; then find \"$root/dev\" -mindepth 1 -maxdepth 1 \\( -type d -o -type l \\) -print; fi; }} | LC_ALL=C sort | while IFS= read -r p; do rel=${{p#\"$root\"/}}; binary=; case \"$rel\" in homeboy-*) binary=\"$p/target/release/homeboy\" ;; dev/[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) binary=\"$p/homeboy\" ;; esac; blocks=$(du -sk \"$p\" 2>/dev/null | cut -f1); bytes=$(( ${{blocks:-0}} * 1024 )); mtime=$(stat -c %Y \"$p\" 2>/dev/null || stat -f %m \"$p\" 2>/dev/null || echo 0); age=$((now-mtime)); [ \"$age\" -ge 0 ] || age=0; identity=$(stat -c '%d:%i:%Y' \"$p\" 2>/dev/null || stat -f '%d:%i:%m' \"$p\" 2>/dev/null || echo unknown); [ -n \"$binary\" ] && [ -f \"$binary\" ] && [ ! -L \"$binary\" ] || {{ printf '%s\\t%s\\t%s\\t%s\\tretained\\tunrecognized_slot\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; continue; }}; [ ! -L \"$p\" ] || {{ printf '%s\\t%s\\t%s\\t%s\\tretained\\tsymlink\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; continue; }}; case \"$configured\" in \"$p\"/*) printf '%s\\t%s\\t%s\\t%s\\tselected\\tconfigured_homeboy_path\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; continue ;; esac; command -v lsof >/dev/null 2>&1 || {{ printf '%s\\t%s\\t%s\\t%s\\tambiguous\\tprocess_probe_unavailable\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; continue; }}; if lsof -w -Fn -a -d cwd +D \"$p\" 2>/dev/null | grep -q . || lsof -w -Fn +D \"$p\" 2>/dev/null | grep -q .; then printf '%s\\t%s\\t%s\\t%s\\tactive\\tprocess_ownership\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; elif [ \"$age\" -lt \"$min_age\" ]; then printf '%s\\t%s\\t%s\\t%s\\tretained\\tminimum_age\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; else printf '%s\\t%s\\t%s\\t%s\\teligible\\tunselected_managed_slot\\n' \"$p\" \"$bytes\" \"$age\" \"$identity\"; fi; done",
        root = shell::quote_arg(root), configured = shell::quote_arg(configured), min_age = min_age,
    )
}

fn delete_command(root: &str, path: &str, identity: &str, configured: &str) -> String {
    format!(
        "root={root}; p={path}; expected={identity}; configured={configured}; binary=; case \"$p\" in \"$root\"/homeboy-*) binary=\"$p/target/release/homeboy\" ;; \"$root\"/dev/[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) binary=\"$p/homeboy\" ;; *) printf changed_identity; exit 0;; esac; [ ! -L \"$p\" ] && [ -d \"$p\" ] && [ -f \"$binary\" ] && [ ! -L \"$binary\" ] || {{ printf symlink_or_missing; exit 0; }}; case \"$configured\" in \"$p\"/*) printf selected; exit 0 ;; esac; actual=$(stat -c '%d:%i:%Y' \"$p\" 2>/dev/null || stat -f '%d:%i:%m' \"$p\" 2>/dev/null || echo unknown); [ \"$actual\" = \"$expected\" ] || {{ printf changed_identity; exit 0; }}; command -v lsof >/dev/null 2>&1 || {{ printf process_probe_unavailable; exit 0; }}; if lsof -w -Fn -a -d cwd +D \"$p\" 2>/dev/null | grep -q . || lsof -w -Fn +D \"$p\" 2>/dev/null | grep -q .; then printf active_process; exit 0; fi; rm -rf -- \"$p\" && printf removed || printf delete_failed",
        root = shell::quote_arg(root), path = shell::quote_arg(path), identity = shell::quote_arg(identity), configured = shell::quote_arg(configured),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn run_shell(command: &str, path: &str) -> std::process::Output {
        Command::new("sh")
            .args(["-c", command])
            .env("PATH", path)
            .output()
            .expect("run cache lifecycle shell")
    }

    #[cfg(unix)]
    fn inactive_process_tools(root: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;

        let tools = root.join("tools");
        fs::create_dir_all(&tools).expect("create process tools");
        for name in ["ps", "lsof"] {
            let path = tools.join(name);
            fs::write(&path, "#!/bin/sh\nexit 1\n").expect("write process tool");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("make process tool executable");
        }
        format!(
            "{}:{}",
            tools.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    #[cfg(unix)]
    fn active_process_tools(root: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;

        let tools = root.join("active-tools");
        fs::create_dir_all(&tools).expect("create process tools");
        let lsof = tools.join("lsof");
        fs::write(&lsof, "#!/bin/sh\nprintf 'n%s\\n' \"$5\"\n").expect("write lsof tool");
        fs::set_permissions(&lsof, fs::Permissions::from_mode(0o755))
            .expect("make lsof tool executable");
        format!(
            "{}:{}",
            tools.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn refresh_slot(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let slot = root.join(name);
        fs::create_dir_all(slot.join("target/release")).expect("create refresh slot");
        fs::write(slot.join("target/release/homeboy"), b"binary").expect("write binary");
        slot
    }

    #[test]
    #[cfg(unix)]
    fn inventory_classifies_real_managed_and_ambiguous_layouts() {
        let temp = TempDir::new().expect("temp root");
        let path = inactive_process_tools(temp.path());
        let root = temp.path().join("_homeboy_binaries");
        let selected = refresh_slot(&root, "homeboy-current");
        let eligible = root.join("dev/0123456789abcdef");
        fs::create_dir_all(&eligible).expect("create dev slot");
        fs::write(eligible.join("homeboy"), b"binary").expect("write dev binary");
        let malformed = root.join("homeboy-malformed");
        fs::create_dir_all(&malformed).expect("create malformed slot");

        let command = inventory_command(
            root.to_str().expect("root path"),
            selected
                .join("target/release/homeboy")
                .to_str()
                .expect("selected path"),
            0,
        );
        let output = run_shell(&command, &path);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let entries = parse_entries(&String::from_utf8_lossy(&output.stdout)).expect("inventory");

        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|entry| {
            entry.path == selected.to_string_lossy()
                && entry.state == "selected"
                && entry.reason == "configured_homeboy_path"
        }));
        assert!(
            entries.iter().any(|entry| {
                entry.path == eligible.to_string_lossy() && entry.state == "eligible"
            }),
            "{entries:#?}"
        );
        assert!(entries.iter().any(|entry| {
            entry.path == malformed.to_string_lossy()
                && entry.state == "retained"
                && entry.reason == "unrecognized_slot"
        }));
    }

    #[test]
    #[cfg(unix)]
    fn delete_removes_exact_candidate_and_preserves_replaced_path() {
        let temp = TempDir::new().expect("temp root");
        let path = inactive_process_tools(temp.path());
        let root = temp.path().join("_homeboy_binaries");
        let removable = refresh_slot(&root, "homeboy-old");
        let inventory = run_shell(
            &inventory_command(root.to_str().expect("root path"), "", 0),
            &path,
        );
        let entry = parse_entries(&String::from_utf8_lossy(&inventory.stdout))
            .expect("inventory")
            .into_iter()
            .find(|entry| entry.path == removable.to_string_lossy())
            .expect("removable entry");
        let removed = run_shell(
            &delete_command(
                root.to_str().expect("root path"),
                removable.to_str().expect("slot path"),
                &entry.identity,
                "",
            ),
            &path,
        );
        assert_eq!(String::from_utf8_lossy(&removed.stdout), "removed");
        assert!(!removable.exists());

        let replaced = refresh_slot(&root, "homeboy-replaced");
        let inventory = run_shell(
            &inventory_command(root.to_str().expect("root path"), "", 0),
            &path,
        );
        let entry = parse_entries(&String::from_utf8_lossy(&inventory.stdout))
            .expect("inventory")
            .into_iter()
            .find(|entry| entry.path == replaced.to_string_lossy())
            .expect("replaceable entry");
        let moved = root.join("moved");
        fs::rename(&replaced, &moved).expect("replace candidate");
        refresh_slot(&root, "homeboy-replaced");
        let changed = run_shell(
            &delete_command(
                root.to_str().expect("root path"),
                replaced.to_str().expect("slot path"),
                &entry.identity,
                "",
            ),
            &path,
        );
        assert_eq!(String::from_utf8_lossy(&changed.stdout), "changed_identity");
        assert!(replaced.exists());
    }

    #[test]
    #[cfg(unix)]
    fn inventory_and_delete_preserve_process_owned_slot() {
        let temp = TempDir::new().expect("temp root");
        let path = active_process_tools(temp.path());
        let root = temp.path().join("_homeboy_binaries");
        let active = refresh_slot(&root, "homeboy-active");
        let inventory = run_shell(
            &inventory_command(root.to_str().expect("root path"), "", 0),
            &path,
        );
        let entry = parse_entries(&String::from_utf8_lossy(&inventory.stdout))
            .expect("inventory")
            .into_iter()
            .find(|entry| entry.path == active.to_string_lossy())
            .expect("active entry");

        assert_eq!(entry.state, "active");
        assert_eq!(entry.reason, "process_ownership");
        let deletion = run_shell(
            &delete_command(
                root.to_str().expect("root path"),
                active.to_str().expect("slot path"),
                &entry.identity,
                "",
            ),
            &path,
        );
        assert_eq!(String::from_utf8_lossy(&deletion.stdout), "active_process");
        assert!(active.exists());
    }

    #[test]
    #[cfg(unix)]
    fn inventory_and_delete_preserve_symlinked_slot() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("temp root");
        let path = inactive_process_tools(temp.path());
        let root = temp.path().join("_homeboy_binaries");
        fs::create_dir_all(&root).expect("cache root");
        let target = refresh_slot(temp.path(), "target");
        let linked = root.join("homeboy-linked");
        symlink(&target, &linked).expect("slot symlink");
        let inventory = run_shell(
            &inventory_command(root.to_str().expect("root path"), "", 0),
            &path,
        );
        let entry = parse_entries(&String::from_utf8_lossy(&inventory.stdout))
            .expect("inventory")
            .into_iter()
            .find(|entry| entry.path == linked.to_string_lossy())
            .expect("linked entry");

        assert_eq!(entry.state, "retained");
        assert_eq!(entry.reason, "symlink");
        let deletion = run_shell(
            &delete_command(
                root.to_str().expect("root path"),
                linked.to_str().expect("slot path"),
                &entry.identity,
                "",
            ),
            &path,
        );
        assert_eq!(
            String::from_utf8_lossy(&deletion.stdout),
            "symlink_or_missing"
        );
        assert!(linked.exists());
        assert!(target.exists());
    }
}
