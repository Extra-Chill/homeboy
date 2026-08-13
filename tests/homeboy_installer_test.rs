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
    fs::write(path, contents).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

fn archive(root: &Path, destination: &Path, fixture: ArchiveFixture) {
    let target = root.join(TARGET);
    fs::create_dir_all(&target).expect("target directory");
    let candidate = target.join("homeboy");
    write_executable(
        &candidate,
        "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = upgrade-admission ]; then\n  printf '%s|%s\\n' \"$0\" \"$4\" > \"$HOMEBOY_TEST_EVIDENCE\"\n  printf 'admission\\n' >> \"$HOMEBOY_TEST_EVENTS\"\n  exit ${HOMEBOY_TEST_CANDIDATE_EXIT:-0}\nfi\nexit 64\n",
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
        command.arg("-s").arg(transform);
    }
    command.args(entries);
    assert!(command.status().expect("create archive").success());
}

fn run_installer(
    fixture: ArchiveFixture,
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
        .env("HOMEBOY_TEST_CANDIDATE_EXIT", candidate_exit.to_string())
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
    let (allowed, installed, evidence, _) = run_installer(ArchiveFixture::Valid, 0, true, false);
    assert!(allowed.status.success(), "{allowed:?}");
    assert!(installed.contains("upgrade-admission"));
    assert!(evidence.contains("legacy-controller-identity"));
    assert!(evidence
        .split('|')
        .next()
        .is_some_and(|candidate| candidate.ends_with("/homeboy")));

    for exit_code in [1, 70] {
        let (blocked, installed, evidence, _) =
            run_installer(ArchiveFixture::Valid, exit_code, true, false);
        assert!(!blocked.status.success());
        assert!(installed.contains("legacy-controller-identity"));
        assert!(evidence.contains("legacy-controller-identity"));
    }
}

#[test]
fn installer_creates_a_first_install_parent_before_staging_the_replacement() {
    let (output, installed, evidence, _) = run_installer(ArchiveFixture::Valid, 0, false, false);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("upgrade-admission"));
    assert!(evidence.contains("unavailable"));
}

#[test]
fn installer_uses_privileged_atomic_replacement_after_staged_admission() {
    let (output, installed, evidence, events) = run_installer(ArchiveFixture::Valid, 0, true, true);

    assert!(output.status.success(), "{output:?}");
    assert!(installed.contains("upgrade-admission"));
    assert!(evidence.contains("legacy-controller-identity"));
    assert_eq!(
        events.lines().collect::<Vec<_>>(),
        ["admission", "sudo:install", "sudo:mv"]
    );
}

#[test]
fn installer_rejects_unsafe_or_ambiguous_archive_members_before_admission() {
    for fixture in [
        ArchiveFixture::MissingCandidate,
        ArchiveFixture::AdditionalCandidate,
        ArchiveFixture::Traversal,
        ArchiveFixture::Absolute,
        ArchiveFixture::SymlinkCandidate,
        ArchiveFixture::HardlinkCandidate,
    ] {
        let (output, installed, evidence, events) = run_installer(fixture, 0, true, false);
        assert!(!output.status.success(), "{output:?}");
        assert!(installed.contains("legacy-controller-identity"));
        assert!(evidence.is_empty());
        assert!(events.is_empty());
    }
}
