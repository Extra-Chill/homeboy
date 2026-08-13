use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

fn run_installer(
    candidate_exit: i32,
    legacy_installed: bool,
    force_sudo: bool,
) -> (std::process::Output, String, String, String) {
    let temp = tempfile::tempdir().expect("tempdir");
    let tools = temp.path().join("tools");
    let install_dir = temp.path().join("missing-parent/bin");
    fs::create_dir_all(&tools).expect("tools directory");
    let installed = install_dir.join("homeboy");
    let evidence = temp.path().join("admission-evidence");
    let events = temp.path().join("events");
    let candidate = temp.path().join("candidate");
    write_executable(
        &candidate,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = upgrade-admission ]; then\n  printf '%s|%s\\n' \"$0\" \"$4\" > \"$HOMEBOY_TEST_EVIDENCE\"\n  printf 'admission\\n' >> \"$HOMEBOY_TEST_EVENTS\"\n  exit {candidate_exit}\nfi\nexit 64\n"
        ),
    );
    if legacy_installed {
        fs::create_dir_all(&install_dir).expect("install directory");
        write_executable(
            &installed,
            "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then printf 'legacy-controller-identity'; exit 0; fi\nexit 64\n",
        );
    }
    write_executable(
        &tools.join("curl"),
        "#!/bin/sh\nout=\nfor arg in \"$@\"; do [ \"$previous\" = -o ] && out=\"$arg\"; previous=\"$arg\"; done\ncase \"$out\" in *.sha256) printf 'unused  homeboy.tar.xz\\n' > \"$out\" ;; *) : > \"$out\" ;; esac\n",
    );
    write_executable(
        &tools.join("sha256sum"),
        "#!/bin/sh\nprintf 'homeboy.tar.xz: OK\\n'\n",
    );
    write_executable(
        &tools.join("tar"),
        "#!/bin/sh\ncp \"$HOMEBOY_TEST_CANDIDATE\" homeboy\n",
    );
    write_executable(
        &tools.join("sudo"),
        "#!/bin/sh\nprintf 'sudo:%s\\n' \"$1\" >> \"$HOMEBOY_TEST_EVENTS\"\nchmod u+w \"$HOMEBOY_TEST_BIN_DIR\"\n\"$@\"\n",
    );
    let mut command = Command::new("sh");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/homeboy-installer.sh"
        ))
        .env("HOMEBOY_INSTALL_PATH", &installed)
        .env("HOMEBOY_TEST_CANDIDATE", &candidate)
        .env("HOMEBOY_TEST_EVIDENCE", &evidence)
        .env("HOMEBOY_TEST_EVENTS", &events)
        .env("HOMEBOY_TEST_BIN_DIR", &install_dir)
        .env("PATH", format!("{}:/usr/bin:/bin", tools.display()));
    if force_sudo {
        command.env("HOMEBOY_INSTALL_USE_SUDO", "true");
    }
    let output = command.output().expect("run installer");
    let installed_bytes = fs::read_to_string(&installed).unwrap_or_default();
    let evidence = fs::read_to_string(&evidence).unwrap_or_default();
    let events = fs::read_to_string(&events).unwrap_or_default();
    (output, installed_bytes, evidence, events)
}

#[test]
fn installer_admits_a_staged_candidate_and_preserves_bytes_on_admission_failure() {
    let (allowed, installed, evidence, _) = run_installer(0, true, false);
    assert!(allowed.status.success(), "{allowed:?}");
    assert!(installed.contains("upgrade-admission"));
    assert!(evidence.contains("legacy-controller-identity"));
    assert!(evidence.contains("/homeboy"));

    for exit_code in [1, 70] {
        let (blocked, installed, evidence, _) = run_installer(exit_code, true, false);
        assert!(!blocked.status.success());
        assert!(installed.contains("legacy-controller-identity"));
        assert!(evidence.contains("legacy-controller-identity"));
    }
}

#[test]
fn installer_creates_a_first_install_parent_before_staging_the_replacement() {
    let (output, installed, evidence, _) = run_installer(0, false, false);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("upgrade-admission"));
    assert!(evidence.contains("unavailable"));
}

#[test]
fn installer_uses_privileged_atomic_replacement_after_staged_admission() {
    let (output, installed, evidence, events) = run_installer(0, true, true);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("upgrade-admission"));
    assert!(evidence.contains("legacy-controller-identity"));
    assert_eq!(
        events.lines().collect::<Vec<_>>(),
        ["admission", "sudo:install", "sudo:mv"]
    );
}
