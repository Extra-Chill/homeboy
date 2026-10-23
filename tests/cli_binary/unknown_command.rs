//! An unrecognized top-level command is a parse error, and must cost what a
//! parse error costs. The clap-error augmenter suggests entity IDs; it used to
//! build them from the resolved component inventory, which opens every attached
//! checkout and spawns Git and GitHub lookups per component. On a populated
//! config `homeboy <typo>` hung for minutes before printing clap's error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(unix)]
#[test]
fn unknown_top_level_command_fails_without_resolving_components() {
    let fixture = Fixture::new();

    let output = fixture.run(&["nonexistentxyz"]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        fixture.describe(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized subcommand 'nonexistentxyz'") && stderr.contains("--help"),
        "missing clap diagnostic and help hint: {stderr}"
    );
    fixture.assert_no_subprocess_lookups();
}

#[cfg(unix)]
#[test]
fn unknown_top_level_command_still_suggests_registered_components() {
    let fixture = Fixture::new();

    let output = fixture.run(&["slow-plugn"]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        fixture.describe(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Did you mean component 'slow-plugin'?"),
        "registry-backed component suggestion missing: {stderr}"
    );
    fixture.assert_no_subprocess_lookups();
}

struct Fixture {
    home: tempfile::TempDir,
    bin: PathBuf,
    lookup_log: PathBuf,
}

impl Fixture {
    /// A project with one attached portable component whose `homeboy.json`
    /// omits `remote_url`: resolving it runs `git remote get-url origin`.
    /// `git` and `gh` are shims that log any invocation.
    fn new() -> Self {
        let home = tempfile::tempdir().expect("temporary home");
        let checkout = home.path().join("checkouts/slow-plugin");
        fs::create_dir_all(&checkout).expect("component checkout");
        fs::write(
            checkout.join("homeboy.json"),
            serde_json::json!({ "id": "slow-plugin" }).to_string(),
        )
        .expect("portable component config");

        let project_dir = home.path().join(".config/homeboy/projects/site");
        fs::create_dir_all(&project_dir).expect("project directory");
        fs::write(
            project_dir.join("site.json"),
            serde_json::json!({
                "id": "site",
                "components": [{
                    "id": "slow-plugin",
                    "local_path": checkout,
                    "remote_path": "wp-content/plugins/slow-plugin"
                }]
            })
            .to_string(),
        )
        .expect("project config");

        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).expect("shim directory");
        let lookup_log = home.path().join("subprocess-lookups.log");
        for tool in ["git", "gh"] {
            write_logging_shim(&bin.join(tool), tool, &lookup_log);
        }

        Self {
            home,
            bin,
            lookup_log,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(homeboy_bin())
            .args(args)
            .current_dir(self.home.path())
            .env_clear()
            .env("HOME", self.home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output()
            .expect("run shipped Homeboy binary")
    }

    fn assert_no_subprocess_lookups(&self) {
        let lookups = fs::read_to_string(&self.lookup_log).unwrap_or_default();
        assert!(
            lookups.is_empty(),
            "an unrecognized command ran Git/GitHub lookups:\n{lookups}"
        );
    }

    fn describe(&self, output: &Output) -> String {
        format!(
            "status: {:?}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

#[cfg(unix)]
fn write_logging_shim(path: &Path, tool: &str, log: &Path) {
    homeboy_test_support::write_executable(
        path,
        format!(
            "#!/bin/sh\necho \"{tool} $*\" >> '{}'\nexit 1\n",
            log.display()
        ),
        0o755,
    )
    .expect("write shim");
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
