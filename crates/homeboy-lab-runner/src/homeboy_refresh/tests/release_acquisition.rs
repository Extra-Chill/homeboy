//! Runner release acquisition and managed-slot provenance (#15774).
//!
//! Each test drives the real materialization script against a local fixture
//! repository. Release downloads go through a `curl` shim that serves assets
//! from a fixture directory, and the canonical source URL is redirected to the
//! fixture repository with a Git `insteadOf` rule, so no test reaches the
//! network or a real runner.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CANONICAL_SOURCE: &str = "https://github.com/Extra-Chill/homeboy.git";
const RELEASE_TAG: &str = "v9.9.9";

/// A fake Homeboy binary. `self identity` is pretty-printed over several
/// lines, as the real command is.
const FAKE_BINARY_TEMPLATE: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'homeboy 9.9.9+@COMMIT@'; exit 0; fi
if [ "$1 $2" = 'self identity' ]; then
  printf '%s\n' '{' '  "success": true,' '  "data": {' '    "version": "9.9.9",' '    "display": "homeboy 9.9.9+@COMMIT@",' '    "git_commit": "@COMMIT@",' '    "git_dirty": false' '  }' '}'
  exit 0
fi
exit 64
"#;

#[test]
fn upgrade_recovery_ref_passes_refresh_validation() {
    let commands = crate::upgrade_runners::managed_immutable_runner_recovery_commands_with_commit(
        "lab",
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    );
    let args: Vec<_> = commands[0].split_whitespace().collect();
    let tag = args[args.iter().position(|arg| *arg == "--ref").unwrap() + 1].trim_matches('\'');
    assert!(release_version(tag).is_ok(), "{tag}");
}

#[test]
fn slot_provenance_matches_content_not_acquisition_metadata() {
    let Some(triple) = host_release_triple() else {
        return;
    };
    let fixture = release_fixture();
    publish_release_archive(&fixture, triple, ReleaseArchive::Valid);
    let root = binaries_root(&fixture);
    let dir = root.join("acquisition");
    let first = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let binary = refreshed_binary_path(
        &ssh_bootstrap_plan(),
        &String::from_utf8_lossy(&first.stdout),
    )
    .unwrap();
    let provenance = Path::new(&binary).parent().unwrap().join("provenance");
    let original = fs::read_to_string(&provenance).unwrap();
    let mut fields: Vec<_> = original.lines().map(str::to_string).collect();
    fields[0] = "different source".into();
    fields[1] = "different cache".into();
    fields[2] = "different ref".into();
    fields[3] = fixture.commit.clone();
    fs::write(&provenance, format!("{}\n", fields.join("\n"))).unwrap();
    let reused = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
    assert!(
        reused.status.success(),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
    assert_eq!(
        refreshed_binary_path(
            &ssh_bootstrap_plan(),
            &String::from_utf8_lossy(&reused.stdout)
        )
        .unwrap(),
        binary
    );
    for index in [4, 5] {
        let mut mismatched = fields.clone();
        mismatched[index] = "different content".into();
        fs::write(&provenance, format!("{}\n", mismatched.join("\n"))).unwrap();
        let refused = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr)
            .contains("immutable Homeboy slot provenance mismatch"));
    }
}

#[derive(Clone, Copy)]
enum ReleaseArchive {
    Valid,
    SymlinkCandidate,
    TraversalMember,
}

struct ReleaseFixture {
    root: tempfile::TempDir,
    source: PathBuf,
    tools: PathBuf,
    releases: PathBuf,
    cargo_log: PathBuf,
    template: PathBuf,
    commit: String,
}

fn host_release_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

fn run(command: &mut Command) -> String {
    let output = command.output().expect("run fixture command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn write_executable(path: &Path, contents: &str) {
    homeboy_test_support::write_executable(path, contents, 0o755).expect("write executable");
}

fn fake_binary(commit: &str) -> String {
    FAKE_BINARY_TEMPLATE.replace("@COMMIT@", commit)
}

fn release_fixture() -> ReleaseFixture {
    let root = tempfile::tempdir().expect("fixture directory");
    let source = root.path().join("source");
    let tools = root.path().join("tools");
    let releases = root.path().join("releases");
    fs::create_dir_all(&source).expect("source directory");
    fs::create_dir_all(&tools).expect("tools directory");
    fs::create_dir_all(&releases).expect("releases directory");
    for args in [
        vec!["init", "--quiet", "--initial-branch=main"],
        vec!["config", "user.name", "Homeboy Test"],
        vec!["config", "user.email", "homeboy@example.test"],
    ] {
        run(Command::new("git").args(args).current_dir(&source));
    }
    fs::write(source.join("README.md"), "fixture\n").expect("fixture source");
    run(Command::new("git").args(["add", "."]).current_dir(&source));
    run(Command::new("git")
        .args(["commit", "-q", "-m", "fixture"])
        .current_dir(&source));
    run(Command::new("git")
        .args(["tag", "-a", RELEASE_TAG, "-m", "release"])
        .current_dir(&source));
    let commit = run(Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&source));

    let template = root.path().join("binary-template");
    fs::write(&template, FAKE_BINARY_TEMPLATE).expect("binary template");
    let cargo_log = root.path().join("cargo.log");
    write_executable(
        &tools.join("cargo"),
        "#!/bin/sh\nset -e\nwhile [ \"$1\" != \"--manifest-path\" ]; do shift; done\ndir=$(dirname \"$2\")\necho cargo >> \"$HOMEBOY_TEST_CARGO_LOG\"\ncommit=$(git -C \"$dir\" rev-parse HEAD)\nmkdir -p \"$dir/target/release\"\nsed \"s/@COMMIT@/$commit/g\" \"$HOMEBOY_TEST_BINARY_TEMPLATE\" > \"$dir/target/release/homeboy\"\nchmod 0755 \"$dir/target/release/homeboy\"\n",
    );
    write_executable(
        &tools.join("curl"),
        "#!/bin/sh\nout=\nurl=\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=$2; shift 2;; https://*) url=$1; shift;; *) shift;; esac; done\ncp \"$HOMEBOY_TEST_RELEASES/${url#https://github.com/Extra-Chill/homeboy/releases/download/}\" \"$out\"\n",
    );
    ReleaseFixture {
        root,
        source,
        tools,
        releases,
        cargo_log,
        template,
        commit,
    }
}

fn publish_release_archive(fixture: &ReleaseFixture, triple: &str, kind: ReleaseArchive) {
    let stage = fixture.root.path().join("archive-root");
    let member = format!("homeboy-{triple}");
    fs::create_dir_all(stage.join(&member)).expect("archive member directory");
    let candidate = stage.join(&member).join("homeboy");
    let mut entries = vec![member.clone()];
    let mut transform = None;
    match kind {
        ReleaseArchive::Valid => write_executable(&candidate, &fake_binary(&fixture.commit)),
        ReleaseArchive::SymlinkCandidate => {
            let elsewhere = fixture.root.path().join("elsewhere");
            write_executable(&elsewhere, &fake_binary(&fixture.commit));
            std::os::unix::fs::symlink(&elsewhere, &candidate).expect("symlink candidate");
        }
        ReleaseArchive::TraversalMember => {
            write_executable(&candidate, &fake_binary(&fixture.commit));
            fs::write(stage.join("payload"), "unsafe").expect("payload");
            entries.push("payload".to_string());
            transform = Some(",^payload$,../payload,");
        }
    }
    let tag_dir = fixture.releases.join(RELEASE_TAG);
    fs::create_dir_all(&tag_dir).expect("release tag directory");
    let asset_name = format!("homeboy-{triple}.tar.xz");
    let asset = tag_dir.join(&asset_name);
    let mut tar = Command::new("tar");
    tar.arg("-cJf").arg(&asset).arg("-C").arg(&stage);
    if let Some(transform) = transform {
        let gnu_tar = Command::new("tar")
            .arg("--version")
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("GNU tar"));
        if gnu_tar {
            tar.arg(format!("--transform=s{transform}"));
        } else {
            tar.arg("-s").arg(transform);
        }
    }
    tar.args(entries);
    run(&mut tar);
    let sha = run(Command::new("sh").arg("-c").arg(format!(
        "(sha256sum '{0}' 2>/dev/null || shasum -a 256 '{0}') | awk '{{print $1}}'",
        asset.display()
    )));
    fs::write(
        tag_dir.join(format!("{asset_name}.sha256")),
        format!("{sha}  {asset_name}\n"),
    )
    .expect("release checksum");
}

fn fixture_shell(fixture: &ReleaseFixture, script: &str) -> Command {
    let mut command = Command::new("bash");
    command
        .args(["-c", script])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fixture.tools.display(),
                std::env::var("PATH").expect("PATH")
            ),
        )
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", fixture.source.display()),
        )
        .env("GIT_CONFIG_VALUE_0", CANONICAL_SOURCE)
        .env("HOMEBOY_TEST_RELEASES", &fixture.releases)
        .env("HOMEBOY_TEST_CARGO_LOG", &fixture.cargo_log)
        .env("HOMEBOY_TEST_BINARY_TEMPLATE", &fixture.template);
    for key in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "HOMEBOY_CARGO_TARGET_DIR",
    ] {
        command.env_remove(key);
    }
    command
}

fn materialize(fixture: &ReleaseFixture, source: &str, git_ref: &str, dir: &Path) -> Output {
    let script = materialize_script(
        source,
        git_ref,
        dir.to_str().expect("target dir"),
        dir.join("target/release/homeboy")
            .to_str()
            .expect("binary path"),
    );
    fixture_shell(fixture, &script)
        .output()
        .expect("run materialization")
}

fn binaries_root(fixture: &ReleaseFixture) -> PathBuf {
    let root = fixture.root.path().join("workspace/_homeboy_binaries");
    fs::create_dir_all(&root).expect("binaries root");
    root
}

fn select_probe(fixture: &ReleaseFixture, binary: &str) -> String {
    let output = fixture_shell(fixture, &identity_probe_script(binary))
        .output()
        .expect("probe selected slot");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("probe output")
}

/// Slot directories other than the binary's own content-addressed slot.
fn published_slots(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .expect("read binaries root")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("homeboy-") && name.len() == "homeboy-".len() + 64)
        .collect()
}

#[test]
fn release_acquisition_publishes_a_verified_slot_without_a_git_checkout() {
    let Some(triple) = host_release_triple() else {
        return;
    };
    let fixture = release_fixture();
    publish_release_archive(&fixture, triple, ReleaseArchive::Valid);
    let root = binaries_root(&fixture);
    let dir = root.join(format!("homeboy-{RELEASE_TAG}"));

    let output = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("materialize output");
    assert!(
        !dir.exists(),
        "a released tag is acquired without creating the git checkout {}",
        dir.display()
    );
    assert!(
        !fixture.cargo_log.exists(),
        "a released tag is never built from source"
    );
    let immutable = refreshed_binary_path(&ssh_bootstrap_plan(), &stdout).expect("immutable path");
    let slot = Path::new(&immutable).parent().expect("slot");
    assert!(slot.join("provenance").is_file());
    assert_eq!(published_slots(&root).len(), 1);
    assert!(
        fs::read_dir(&root)
            .expect("read binaries root")
            .filter_map(|entry| entry.ok())
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".stage-")),
        "the installer stage is removed after slot publication"
    );
    let identity = parse_identity(&stdout).expect("release identity");
    let materialize_plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: Some(CANONICAL_SOURCE.to_string()),
        git_ref: Some(RELEASE_TAG.to_string()),
        target_dir: Some(dir.display().to_string()),
        binary_path: dir.join("target/release/homeboy").display().to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    verify_materialized_identity(&materialize_plan, &stdout, &identity)
        .expect("release identity matches the resolved tag commit");
    assert!(!root.join(".homeboy-history").exists());
    parse_identity(&select_probe(&fixture, &immutable)).expect("selected identity");
}

#[test]
fn acquired_release_tag_reuses_its_verified_slot_without_downloading() {
    let Some(triple) = host_release_triple() else {
        return;
    };
    let fixture = release_fixture();
    publish_release_archive(&fixture, triple, ReleaseArchive::Valid);
    let root = binaries_root(&fixture);
    let dir = root.join(format!("homeboy-{RELEASE_TAG}"));
    // A marker naming a slot that does not verify is ignored.
    fs::write(
        root.join(format!(".release-{RELEASE_TAG}.sha256")),
        "0".repeat(64),
    )
    .unwrap();

    let first = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = String::from_utf8(first.stdout).unwrap();
    assert!(
        first.contains(&format!(
            "HOMEBOY_REFRESH_BINARY_ORIGIN=release:{RELEASE_TAG}\n"
        )),
        "{first}"
    );

    // The release asset is gone: only the recorded, re-verified slot can satisfy this.
    fs::remove_dir_all(fixture.releases.join(RELEASE_TAG)).unwrap();
    let second = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = String::from_utf8(second.stdout).unwrap();
    assert!(
        second.contains(&format!(
            "HOMEBOY_REFRESH_BINARY_ORIGIN=release-cache:{RELEASE_TAG}\n"
        )),
        "{second}"
    );
    assert_eq!(
        refreshed_binary_path(&ssh_bootstrap_plan(), &first).expect("first slot"),
        refreshed_binary_path(&ssh_bootstrap_plan(), &second).expect("reused slot")
    );
}

fn assert_release_archive_rejected(kind: ReleaseArchive, expected: &str) {
    let Some(triple) = host_release_triple() else {
        return;
    };
    let fixture = release_fixture();
    publish_release_archive(&fixture, triple, kind);
    let root = binaries_root(&fixture);
    let dir = root.join(format!("homeboy-{RELEASE_TAG}"));

    let output = materialize(&fixture, CANONICAL_SOURCE, RELEASE_TAG, &dir);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an unsafe release archive must be rejected: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stderr.contains(expected), "{stderr}");
    assert!(
        !fixture.cargo_log.exists(),
        "a rejected release is not replaced by a source build"
    );
    assert!(published_slots(&root).is_empty(), "no slot is published");
}

#[test]
fn release_acquisition_rejects_a_symlink_candidate() {
    assert_release_archive_rejected(
        ReleaseArchive::SymlinkCandidate,
        "Release archive candidate must be a regular file",
    );
}

#[test]
fn release_acquisition_rejects_a_traversal_member() {
    assert_release_archive_rejected(
        ReleaseArchive::TraversalMember,
        "Release archive contains an unsafe member path",
    );
}
