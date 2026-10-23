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

/// Shell functions shared by inventory and delete, so both recognize the same
/// slot layouts and enforce the same protected references.
///
/// Recognized layouts below the cache root:
/// - `homeboy-<64-hex sha256>/homeboy`: content-addressed slot published by refresh.
/// - `homeboy-<ref>/target/release/homeboy`: refresh source checkout.
/// - `dev/<16-hex>/homeboy`: dev-sync native build slot.
///
/// A slot is protected when the configured Homeboy path, any runner-service
/// controller link, or `$HOME/.local/bin/homeboy` resolves into it. Service
/// links are discovered by glob because other controllers' links are unknown
/// to this controller; an unresolvable reference protects every slot.
fn slot_shell_functions(root: &str, configured: &str) -> String {
    const SLOT_FUNCTIONS: &str = r#"slot_binary() {
  case "$1" in
    dev/__HEX16__) printf '%s/homeboy' "$root/$1" ;;
    */*) ;;
    homeboy-__HEX64__) printf '%s/homeboy' "$root/$1" ;;
    homeboy-*) printf '%s/target/release/homeboy' "$root/$1" ;;
  esac
}
reference_protects() {
  [ -n "$2" ] || return 1
  [ -e "$2" ] || [ -L "$2" ] || return 1
  resolved=$(readlink -f "$2" 2>/dev/null) || resolved=
  if [ -z "$resolved" ]; then
    protected_state=ambiguous
    protected_reason="unresolved_$1:$2"
    return 0
  fi
  case "$resolved" in
    "$slot"|"$slot"/*|"$physical"|"$physical"/*)
      protected_state=selected
      protected_reason="$1:$2"
      return 0 ;;
  esac
  return 1
}
slot_protected() {
  slot=$1
  physical=$(cd -P "$slot" 2>/dev/null && pwd -P) || physical=$slot
  case "$configured" in
    "$slot"/*|"$physical"/*)
      protected_state=selected
      protected_reason="configured_homeboy_path:$configured"
      return 0 ;;
  esac
  reference_protects configured_homeboy_path "$configured" && return 0
  for reference in "$HOME"/.local/share/homeboy/runner-service/*/controllers/*/homeboy; do
    reference_protects runner_service_link "$reference" && return 0
  done
  reference_protects local_bin_link "$HOME/.local/bin/homeboy" && return 0
  return 1
}
slot_identity() {
  stat -c '%d:%i:%Y' "$1" 2>/dev/null || stat -f '%d:%i:%m' "$1" 2>/dev/null || echo unknown
}
slot_process_owned() {
  lsof -w -Fn -a -d cwd +D "$1" 2>/dev/null | grep -q . || lsof -w -Fn +D "$1" 2>/dev/null | grep -q .
}
"#;
    format!(
        "root={root}\nconfigured={configured}\n{functions}",
        root = shell::quote_arg(root),
        configured = shell::quote_arg(configured),
        functions = SLOT_FUNCTIONS
            .replace("__HEX16__", &"[0-9a-f]".repeat(16))
            .replace("__HEX64__", &"[0-9a-f]".repeat(64)),
    )
}

fn inventory_command(root: &str, configured: &str, min_age: u64) -> String {
    // `dev-source` stages dev-sync source archives for an in-flight build; it
    // is reported but never pruned. Unrecognized names remain fail-closed.
    format!(
        r#"{functions}min_age={min_age}
now=$(date +%s)
[ -d "$root" ] || exit 0
row() {{ printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$p" "$bytes" "$age" "$identity" "$1" "$2"; }}
{{ find "$root" -mindepth 1 -maxdepth 1 \( -type d -o -type l \) ! -name dev -print; if [ -d "$root/dev" ] && [ ! -L "$root/dev" ]; then find "$root/dev" -mindepth 1 -maxdepth 1 \( -type d -o -type l \) -print; fi; }} | LC_ALL=C sort | while IFS= read -r p; do
  rel=${{p#"$root"/}}
  binary=$(slot_binary "$rel")
  blocks=$(du -sk "$p" 2>/dev/null | cut -f1)
  bytes=$(( ${{blocks:-0}} * 1024 ))
  mtime=$(stat -c %Y "$p" 2>/dev/null || stat -f %m "$p" 2>/dev/null || echo 0)
  age=$((now-mtime))
  [ "$age" -ge 0 ] || age=0
  identity=$(slot_identity "$p")
  if [ "$rel" = dev-source ]; then row retained dev_source_upload_staging; continue; fi
  [ -n "$binary" ] && [ -f "$binary" ] && [ ! -L "$binary" ] || {{ row retained unrecognized_slot; continue; }}
  [ ! -L "$p" ] || {{ row retained symlink; continue; }}
  if slot_protected "$p"; then row "$protected_state" "$protected_reason"; continue; fi
  command -v lsof >/dev/null 2>&1 || {{ row ambiguous process_probe_unavailable; continue; }}
  if slot_process_owned "$p"; then row active process_ownership
  elif [ "$age" -lt "$min_age" ]; then row retained minimum_age
  else row eligible unselected_managed_slot
  fi
done
"#,
        functions = slot_shell_functions(root, configured),
    )
}

fn delete_command(root: &str, path: &str, identity: &str, configured: &str) -> String {
    format!(
        r#"{functions}p={path}
expected={identity}
rel=${{p#"$root"/}}
[ "$rel" != "$p" ] || {{ printf changed_identity; exit 0; }}
binary=$(slot_binary "$rel")
[ -n "$binary" ] || {{ printf changed_identity; exit 0; }}
[ ! -L "$p" ] && [ -d "$p" ] && [ -f "$binary" ] && [ ! -L "$binary" ] || {{ printf symlink_or_missing; exit 0; }}
if slot_protected "$p"; then printf '%s' "$protected_reason"; exit 0; fi
[ "$(slot_identity "$p")" = "$expected" ] || {{ printf changed_identity; exit 0; }}
command -v lsof >/dev/null 2>&1 || {{ printf process_probe_unavailable; exit 0; }}
if slot_process_owned "$p"; then printf active_process; exit 0; fi
rm -rf -- "$p" && printf removed || printf delete_failed
"#,
        functions = slot_shell_functions(root, configured),
        path = shell::quote_arg(path),
        identity = shell::quote_arg(identity),
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
        let tools = root.join("tools");
        fs::create_dir_all(&tools).expect("create process tools");
        for name in ["ps", "lsof"] {
            let path = tools.join(name);
            homeboy_test_support::write_executable(&path, "#!/bin/sh\nexit 1\n", 0o755)
                .expect("write process tool");
        }
        format!(
            "{}:{}",
            tools.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    #[cfg(unix)]
    fn active_process_tools(root: &std::path::Path) -> String {
        let tools = root.join("active-tools");
        fs::create_dir_all(&tools).expect("create process tools");
        let lsof = tools.join("lsof");
        homeboy_test_support::write_executable(&lsof, "#!/bin/sh\nprintf 'n%s\\n' \"$5\"\n", 0o755)
            .expect("write lsof tool");

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

    fn content_addressed_slot(root: &std::path::Path, digit: char) -> std::path::PathBuf {
        let slot = root.join(format!("homeboy-{}", digit.to_string().repeat(64)));
        fs::create_dir_all(&slot).expect("create content-addressed slot");
        fs::write(slot.join("homeboy"), b"binary").expect("write slot binary");
        fs::write(slot.join("provenance"), b"provenance").expect("write provenance");
        slot
    }

    fn run_shell_as(command: &str, path: &str, home: &std::path::Path) -> std::process::Output {
        Command::new("sh")
            .args(["-c", command])
            .env("PATH", path)
            .env("HOME", home)
            .output()
            .expect("run cache lifecycle shell")
    }

    #[test]
    #[cfg(unix)]
    fn content_addressed_slots_prune_only_when_no_live_link_targets_them() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("temp root");
        let path = inactive_process_tools(temp.path());
        let home = temp.path().join("home");
        let root = temp.path().join("_homeboy_binaries");
        let configured = content_addressed_slot(&root, 'a');
        let service = content_addressed_slot(&root, 'b');
        let local_bin = content_addressed_slot(&root, 'c');
        let unreferenced = content_addressed_slot(&root, 'd');
        let service_link =
            home.join(".local/share/homeboy/runner-service/lab/controllers/other/homeboy");
        fs::create_dir_all(service_link.parent().expect("service dir")).expect("service dir");
        symlink(service.join("homeboy"), &service_link).expect("service link");
        let local_bin_link = home.join(".local/bin/homeboy");
        fs::create_dir_all(local_bin_link.parent().expect("bin dir")).expect("bin dir");
        symlink(local_bin.join("homeboy"), &local_bin_link).expect("local bin link");
        let root_arg = root.to_str().expect("root path");
        let configured_binary = configured.join("homeboy");
        let configured_arg = configured_binary.to_str().expect("configured path");

        let output = run_shell_as(
            &inventory_command(root_arg, configured_arg, 0),
            &path,
            &home,
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let entries = parse_entries(&String::from_utf8_lossy(&output.stdout)).expect("inventory");
        let entry = |slot: &std::path::Path| {
            entries
                .iter()
                .find(|entry| entry.path == slot.to_string_lossy())
                .unwrap_or_else(|| panic!("missing {}: {entries:#?}", slot.display()))
        };

        assert_eq!(entries.len(), 4, "{entries:#?}");
        assert_eq!(entry(&configured).state, "selected", "{entries:#?}");
        assert_eq!(
            entry(&configured).reason,
            format!("configured_homeboy_path:{configured_arg}")
        );
        assert_eq!(entry(&service).state, "selected", "{entries:#?}");
        assert_eq!(
            entry(&service).reason,
            format!("runner_service_link:{}", service_link.display())
        );
        assert_eq!(entry(&local_bin).state, "selected", "{entries:#?}");
        assert_eq!(
            entry(&local_bin).reason,
            format!("local_bin_link:{}", local_bin_link.display())
        );
        assert_eq!(entry(&unreferenced).state, "eligible", "{entries:#?}");

        for slot in [&configured, &service, &local_bin] {
            let deletion = run_shell_as(
                &delete_command(
                    root_arg,
                    slot.to_str().expect("slot path"),
                    &entry(slot).identity,
                    configured_arg,
                ),
                &path,
                &home,
            );
            assert_eq!(
                String::from_utf8_lossy(&deletion.stdout),
                entry(slot).reason,
                "{}",
                slot.display()
            );
            assert!(slot.exists());
        }

        let late_link =
            home.join(".local/share/homeboy/runner-service/lab/controllers/late/homeboy");
        fs::create_dir_all(late_link.parent().expect("late dir")).expect("late dir");
        symlink(unreferenced.join("homeboy"), &late_link).expect("late link");
        let refused = run_shell_as(
            &delete_command(
                root_arg,
                unreferenced.to_str().expect("slot path"),
                &entry(&unreferenced).identity,
                configured_arg,
            ),
            &path,
            &home,
        );
        assert_eq!(
            String::from_utf8_lossy(&refused.stdout),
            format!("runner_service_link:{}", late_link.display())
        );
        assert!(unreferenced.exists());
        fs::remove_file(&late_link).expect("remove late link");

        let deletion = run_shell_as(
            &delete_command(
                root_arg,
                unreferenced.to_str().expect("slot path"),
                &entry(&unreferenced).identity,
                configured_arg,
            ),
            &path,
            &home,
        );
        assert_eq!(String::from_utf8_lossy(&deletion.stdout), "removed");
        assert!(!unreferenced.exists());
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
                && entry.reason.starts_with("configured_homeboy_path:")
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
