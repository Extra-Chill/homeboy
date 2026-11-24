//! Converge the runner host's login-shell `homeboy` onto the promoted slot
//! (#17241).
//!
//! Refresh promotes a managed `_homeboy_binaries` slot for the daemon, but an
//! operator who SSHes in runs whatever `homeboy` is first on a login shell's
//! `PATH`. Left alone that copy drifts, and on-host diagnosis reports a
//! different reality than the daemon. After a converged refresh the
//! conventional user install locations are pointed at the same slot, so a host
//! runs one Homeboy version.
//!
//! Only Homeboy-owned targets are touched: a symlink into the managed binaries
//! directory is repointed, and a user-owned regular file that reports an older
//! Homeboy version is replaced. Anything else is reported and left alone.

use homeboy_core::error::posix_quote_arg;
pub use homeboy_upgrade::upgrade::HomeboyPathBinarySync;

/// Report-line marker emitted by [`path_binary_sync_script`].
const REPORT_MARKER: &str = "HOMEBOY_PATH_SYNC";

/// A bash script converging `$HOME/.local/bin/homeboy` and
/// `$HOME/.cargo/bin/homeboy` onto `slot`. Absent locations are ignored.
/// Every touched or refused location prints one tab-separated report line.
pub(crate) fn path_binary_sync_script(slot: &str, binaries_dir: &str) -> String {
    format!(
        r#"set -u
slot={slot}
managed={managed}
report() {{ printf '{REPORT_MARKER}\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${{4:-}}" "${{5:-}}"; }}
version_of() {{ "$1" --version 2>/dev/null | awk 'NR==1 && $1=="homeboy" {{ sub(/\+.*/, "", $2); print $2 }}'; }}
resolve() {{ readlink -f "$1" 2>/dev/null || printf '%s' "$1"; }}
managed_real=$(resolve "$managed")
slot_real=$(resolve "$slot")
slot_version=$(version_of "$slot")
if [ -z "$slot_version" ]; then
  report "$slot" failed "promoted slot did not report a Homeboy version"
  exit 0
fi
for target in "$HOME/.local/bin/homeboy" "$HOME/.cargo/bin/homeboy"; do
  tmp="$target.homeboy-sync.$$"
  if [ -L "$target" ]; then
    resolved=$(resolve "$target")
    case "$resolved" in
      "$managed_real"/*|"$managed"/*) ;;
      *) report "$target" skipped "symlink points outside the managed binaries dir"; continue ;;
    esac
    if [ "$resolved" = "$slot_real" ]; then
      report "$target" current "already points at the promoted slot" "$slot_version" "$slot_version"
      continue
    fi
    current=$(version_of "$target")
    if ln -s "$slot" "$tmp" && mv -f "$tmp" "$target"; then
      report "$target" repointed "$slot" "$current" "$slot_version"
    else
      rm -f "$tmp"
      report "$target" failed "could not repoint the symlink" "$current"
    fi
  elif [ -f "$target" ]; then
    if [ ! -O "$target" ]; then
      report "$target" skipped "not owned by the runner user"
      continue
    fi
    current=$(version_of "$target")
    if [ -z "$current" ]; then
      report "$target" skipped "does not report a Homeboy version"
    elif [ "$current" = "$slot_version" ]; then
      report "$target" current "already runs $slot_version" "$current" "$current"
    elif [ "$(printf '%s\n%s\n' "$current" "$slot_version" | sort -V | tail -n 1)" = "$current" ]; then
      report "$target" skipped "runs $current, newer than the promoted $slot_version" "$current" "$current"
    elif cp "$slot" "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$target"; then
      report "$target" replaced "$current -> $slot_version" "$current" "$slot_version"
    else
      rm -f "$tmp"
      report "$target" failed "could not replace the binary" "$current"
    fi
  fi
done
"#,
        slot = posix_quote_arg(slot),
        managed = posix_quote_arg(binaries_dir.trim_end_matches('/')),
    )
}

/// Parse the report lines of [`path_binary_sync_script`] out of `stdout`.
pub(crate) fn parse_path_binary_sync(stdout: &str) -> Vec<HomeboyPathBinarySync> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim_end_matches('\r').splitn(6, '\t');
            if fields.next()? != REPORT_MARKER {
                return None;
            }
            let mut optional = || {
                fields
                    .next()
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            };
            let path = optional()?;
            let action = optional()?;
            Some(HomeboyPathBinarySync {
                path,
                action,
                detail: optional().unwrap_or_default(),
                version_before: optional(),
                version_after: optional(),
            })
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    fn fake_homeboy(path: &Path, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        homeboy_test_support::write_executable(
            path,
            &format!("#!/bin/sh\necho 'homeboy {version}+abc1234'\n"),
            0o755,
        )
        .unwrap();
    }

    fn version(path: &Path) -> String {
        let output = std::process::Command::new(path)
            .arg("--version")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn run(home: &Path, slot: &Path, binaries: &Path) -> Vec<HomeboyPathBinarySync> {
        let script = path_binary_sync_script(&slot.to_string_lossy(), &binaries.to_string_lossy());
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("HOME", home)
            .output()
            .expect("run sync script");
        assert!(output.status.success(), "{output:?}");
        parse_path_binary_sync(&String::from_utf8_lossy(&output.stdout))
    }

    fn action(reports: &[HomeboyPathBinarySync], path: &Path) -> String {
        reports
            .iter()
            .find(|report| report.path == path.to_string_lossy())
            .map(|report| report.action.clone())
            .unwrap_or_else(|| "absent".to_string())
    }

    #[test]
    fn converges_managed_path_binaries_onto_the_promoted_slot() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let binaries = root.path().join("_homeboy_binaries");
        let slot = binaries.join("homeboy-new/homeboy");
        let old_slot = binaries.join("homeboy-old/homeboy");
        fake_homeboy(&slot, "0.484.0");
        fake_homeboy(&old_slot, "0.426.0");
        let local = home.join(".local/bin/homeboy");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&old_slot, &local).unwrap();
        let cargo = home.join(".cargo/bin/homeboy");
        fake_homeboy(&cargo, "0.482.7");

        let reports = run(&home, &slot, &binaries);
        assert_eq!(action(&reports, &local), "repointed");
        assert_eq!(action(&reports, &cargo), "replaced");
        let cargo_report = reports
            .iter()
            .find(|report| report.path == cargo.to_string_lossy())
            .unwrap();
        assert_eq!(cargo_report.version_before.as_deref(), Some("0.482.7"));
        assert_eq!(cargo_report.version_after.as_deref(), Some("0.484.0"));
        assert_eq!(std::fs::read_link(&local).unwrap(), slot);
        assert_eq!(version(&cargo), "homeboy 0.484.0+abc1234");
        assert_eq!(version(&old_slot), "homeboy 0.426.0+abc1234");

        // A second run is a no-op.
        let reports = run(&home, &slot, &binaries);
        assert_eq!(action(&reports, &local), "current");
        assert_eq!(action(&reports, &cargo), "current");
    }

    #[test]
    fn leaves_unmanaged_and_newer_path_binaries_alone() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let binaries = root.path().join("_homeboy_binaries");
        let slot = binaries.join("homeboy-new/homeboy");
        fake_homeboy(&slot, "0.484.0");
        let elsewhere = root.path().join("elsewhere/homeboy");
        fake_homeboy(&elsewhere, "0.400.0");
        let local = home.join(".local/bin/homeboy");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &local).unwrap();
        let cargo = home.join(".cargo/bin/homeboy");
        fake_homeboy(&cargo, "0.500.0");

        let reports = run(&home, &slot, &binaries);
        assert_eq!(action(&reports, &local), "skipped");
        assert_eq!(action(&reports, &cargo), "skipped");
        assert_eq!(std::fs::read_link(&local).unwrap(), elsewhere);
        assert_eq!(version(&cargo), "homeboy 0.500.0+abc1234");

        // Absent install locations are not reported at all.
        let empty_home = root.path().join("empty");
        assert!(run(&empty_home, &slot, &binaries).is_empty());
    }
}
