use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const TARGET: &str = "homeboy-x86_64-unknown-linux-gnu";

#[derive(Clone, Copy)]
enum ArchiveFixture {
    Valid,
    MissingCandidate,
    AdditionalCandidate,
    Traversal,
    Absolute,
    SymlinkCandidate,
    HardlinkCandidate,
}

fn write_executable(path: &Path, contents: &str) {
    homeboy_test_support::write_executable(path, contents, 0o755).expect("write executable");
}

fn archive(root: &Path, destination: &Path, fixture: ArchiveFixture) {
    let target = root.join(TARGET);
    fs::create_dir_all(&target).expect("target directory");
    let candidate = target.join("homeboy");
    write_executable(
        &candidate,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'homeboy %s\\n' \"${HOMEBOY_TEST_TARGET_VERSION:-0.351.3+fixture}\"; exit 0; fi\nexit 64\n",
    );

    let mut entries = vec![TARGET.to_string()];
    let mut transform = None;
    match fixture {
        ArchiveFixture::Valid => {}
        ArchiveFixture::MissingCandidate => {
            fs::remove_file(&candidate).expect("remove candidate");
            fs::write(target.join("not-homeboy"), "not a candidate").expect("write fixture");
        }
        ArchiveFixture::AdditionalCandidate => {
            let additional = root.join("other");
            fs::create_dir_all(&additional).expect("additional directory");
            write_executable(&additional.join("homeboy"), "#!/bin/sh\nexit 0\n");
            entries.push("other".to_string());
        }
        ArchiveFixture::Traversal | ArchiveFixture::Absolute => {
            fs::remove_dir_all(&target).expect("remove target");
            fs::write(root.join("payload"), "unsafe").expect("write payload");
            entries = vec!["payload".to_string()];
            transform = Some(match fixture {
                ArchiveFixture::Traversal => ",^payload$,../payload,",
                ArchiveFixture::Absolute => ",^payload$,/payload,",
                _ => unreachable!(),
            });
        }
        ArchiveFixture::SymlinkCandidate => {
            fs::remove_file(&candidate).expect("remove candidate");
            std::os::unix::fs::symlink("elsewhere", &candidate).expect("symlink candidate");
        }
        ArchiveFixture::HardlinkCandidate => {
            let original = root.join("original");
            fs::rename(&candidate, &original).expect("move candidate");
            fs::hard_link(&original, &candidate).expect("hardlink candidate");
            entries = vec!["original".to_string(), TARGET.to_string()];
        }
    }

    let mut command = Command::new("tar");
    command.args(["-cJf"]).arg(destination).arg("-C").arg(root);
    if let Some(transform) = transform {
        let gnu_tar = Command::new("tar")
            .arg("--version")
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("GNU tar"));
        if gnu_tar {
            command.arg(format!("--transform=s{transform}"));
        } else {
            command.arg("-s").arg(transform);
        }
    }
    command.args(entries);
    assert!(command.status().expect("create archive").success());
}

fn run_installer(
    fixture: ArchiveFixture,
    legacy_installed: bool,
    force_sudo: bool,
    non_writable_parent: bool,
) -> (std::process::Output, String, String) {
    run_installer_mode(
        fixture,
        legacy_installed,
        force_sudo,
        non_writable_parent,
        false,
    )
}

fn run_installer_mode(
    fixture: ArchiveFixture,
    legacy_installed: bool,
    force_sudo: bool,
    non_writable_parent: bool,
    stage_only: bool,
) -> (std::process::Output, String, String) {
    let temp = tempfile::tempdir().expect("tempdir");
    let tools = temp.path().join("tools");
    let install_dir = temp.path().join("missing-parent/bin");
    fs::create_dir_all(&tools).expect("tools directory");
    let installed = install_dir.join("homeboy");
    let events = temp.path().join("events");
    let archive_path = temp.path().join("homeboy.tar.xz");
    let archive_root = temp.path().join("archive-root");
    archive(&archive_root, &archive_path, fixture);
    if legacy_installed {
        fs::create_dir_all(&install_dir).expect("install directory");
        write_executable(
            &installed,
            "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then printf 'legacy-controller-identity'; exit 0; fi\nexit 64\n",
        );
    }
    if non_writable_parent {
        let mut permissions = fs::metadata(&install_dir)
            .expect("install directory")
            .permissions();
        permissions.set_mode(0o555);
        fs::set_permissions(&install_dir, permissions).expect("make parent non-writable");
    }
    write_executable(
        &tools.join("curl"),
        "#!/bin/sh\nout=\nfor arg in \"$@\"; do [ \"$previous\" = -o ] && out=\"$arg\"; previous=\"$arg\"; done\ncase \"$out\" in *.sha256) printf 'unused  homeboy.tar.xz\\n' > \"$out\" ;; *) cp \"$HOMEBOY_TEST_ARCHIVE\" \"$out\" ;; esac\n",
    );
    write_executable(
        &tools.join("sha256sum"),
        "#!/bin/sh\nprintf 'homeboy.tar.xz: OK\\n'\n",
    );
    write_executable(
        &tools.join("uname"),
        "#!/bin/sh\ncase \"$1\" in -s) printf 'Linux\\n' ;; -m) printf 'x86_64\\n' ;; esac\n",
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
        .env("HOMEBOY_TEST_ARCHIVE", &archive_path)
        .env("HOMEBOY_TEST_EVENTS", &events)
        .env("HOMEBOY_TEST_BIN_DIR", &install_dir)
        .env("HOMEBOY_UPGRADE_RELEASE_TAG", "v0.351.3")
        .env("HOMEBOY_UPGRADE_RELEASE_VERSION", "0.351.3")
        // Archive inspection must be independent of caller tar defaults.
        .env("TAR_OPTIONS", "--invalid-homeboy-installer-option")
        .env("PATH", format!("{}:/usr/bin:/bin", tools.display()));
    if force_sudo {
        command.env("HOMEBOY_INSTALL_USE_SUDO", "true");
    }
    if stage_only {
        command.env("HOMEBOY_INSTALL_STAGE_ONLY", "1");
    }
    let output = command.output().expect("run installer");
    let installed_bytes = fs::read_to_string(&installed).unwrap_or_default();
    let events = fs::read_to_string(&events).unwrap_or_default();
    (output, installed_bytes, events)
}

#[test]
fn installer_rejects_a_digest_verified_candidate_with_the_wrong_target_version() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tools = temp.path().join("tools");
    let install_dir = temp.path().join("bin");
    let installed = install_dir.join("homeboy");
    let archive_path = temp.path().join("homeboy.tar.xz");
    let archive_root = temp.path().join("archive-root");
    fs::create_dir_all(&tools).expect("tools directory");
    fs::create_dir_all(&install_dir).expect("install directory");
    archive(&archive_root, &archive_path, ArchiveFixture::Valid);
    write_executable(&installed, "#!/bin/sh\nprintf 'legacy'\n");
    write_executable(&tools.join("curl"), "#!/bin/sh\nout=\nfor arg in \"$@\"; do [ \"$previous\" = -o ] && out=\"$arg\"; previous=\"$arg\"; done\ncase \"$out\" in *.sha256) printf 'unused  homeboy.tar.xz\\n' > \"$out\" ;; *) cp \"$HOMEBOY_TEST_ARCHIVE\" \"$out\" ;; esac\n");
    write_executable(
        &tools.join("sha256sum"),
        "#!/bin/sh\nprintf 'homeboy.tar.xz: OK\\n'\n",
    );
    write_executable(
        &tools.join("uname"),
        "#!/bin/sh\ncase \"$1\" in -s) printf 'Linux\\n' ;; -m) printf 'x86_64\\n' ;; esac\n",
    );
    let output = Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/homeboy-installer.sh"
        ))
        .env("HOMEBOY_INSTALL_PATH", &installed)
        .env("HOMEBOY_TEST_ARCHIVE", &archive_path)
        .env("HOMEBOY_UPGRADE_RELEASE_VERSION", "9.9.9")
        .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
        .output()
        .expect("run installer");
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(&installed).expect("installed bytes"),
        "#!/bin/sh\nprintf 'legacy'\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Target-version check refused"));
}

#[test]
fn installer_creates_a_first_install_parent_before_staging_the_replacement() {
    let (output, installed, _) = run_installer(ArchiveFixture::Valid, false, false, false);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("0.351.3+fixture"));
}

#[test]
fn installer_uses_privileged_atomic_replacement() {
    let (output, installed, events) = run_installer(ArchiveFixture::Valid, true, true, false);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("0.351.3+fixture"));
    assert_eq!(
        events.lines().collect::<Vec<_>>(),
        ["sudo:install", "sudo:mv"]
    );
}

#[test]
fn installer_uses_privilege_when_binary_is_writable_but_parent_is_not_replaceable() {
    let (output, installed, events) = run_installer(ArchiveFixture::Valid, true, false, true);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("0.351.3+fixture"));
    assert_eq!(
        events.lines().collect::<Vec<_>>(),
        ["sudo:install", "sudo:mv"]
    );
}

#[test]
fn installer_rejects_unsafe_or_ambiguous_archive_members_before_replacement() {
    for fixture in [
        ArchiveFixture::MissingCandidate,
        ArchiveFixture::AdditionalCandidate,
        ArchiveFixture::Traversal,
        ArchiveFixture::Absolute,
        ArchiveFixture::SymlinkCandidate,
        ArchiveFixture::HardlinkCandidate,
    ] {
        let (output, installed, events) = run_installer(fixture, true, false, false);
        assert!(!output.status.success(), "{output:?}");
        assert!(installed.contains("legacy-controller-identity"));
        assert!(events.is_empty());
    }
}

/// `homeboy upgrade` runs this installer in stage-only mode: it places the
/// verified candidate at HOMEBOY_INSTALL_PATH without escalating privilege,
/// because the controller owns installation.
#[test]
fn stage_only_mode_places_the_candidate_without_privilege() {
    let (output, staged, events) =
        run_installer_mode(ArchiveFixture::Valid, false, true, false, true);

    assert!(output.status.success(), "{output:?}");
    assert!(staged.contains("0.351.3+fixture"));
    assert!(
        events.is_empty(),
        "stage-only mode must not use sudo: {events}"
    );
}

#[test]
fn stage_only_mode_requires_an_explicit_install_path() {
    let output = Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/homeboy-installer.sh"
        ))
        .env("HOMEBOY_INSTALL_STAGE_ONLY", "1")
        .env_remove("HOMEBOY_INSTALL_PATH")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run installer");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("HOMEBOY_INSTALL_STAGE_ONLY=1 requires HOMEBOY_INSTALL_PATH"));
}
