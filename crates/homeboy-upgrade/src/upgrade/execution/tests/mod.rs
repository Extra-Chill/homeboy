#![cfg(test)]

mod part_a;
mod part_b;

use super::*;

pub(super) fn checkout_with_package_name(package_name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".git")).expect("git dir");
    write_source_workspace_files(dir.path(), package_name);
    dir
}

pub(super) fn source_workspace_with_package_name(package_name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_source_workspace_files(dir.path(), package_name);
    dir
}

pub(super) fn write_source_workspace_files(path: &Path, package_name: &str) {
    let manifest = serde_json::json!({ "id": package_name });
    std::fs::write(path.join("homeboy.json"), manifest.to_string()).expect("manifest");
    let package_manifest = "Cargo.toml";
    std::fs::write(
        path.join(package_manifest),
        format!("[package]\nname = \"{package_name}\"\nversion = \"0.0.0\"\n"),
    )
    .expect("package manifest");
}

pub(super) fn git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn queued_binary_revalidates_target_before_install() {
    let identity = |version: &str| homeboy_core::build_identity::BuildIdentity {
        version: version.to_string(),
        git_commit: None,
        git_dirty: None,
        display: version.to_string(),
    };

    validate_binary_replacement_eligibility(false, "0.370.0", Some(identity("0.369.0")))
        .expect("older installed controller remains eligible");
    validate_binary_replacement_eligibility(false, "0.370.0", Some(identity("0.370.0")))
        .expect_err("queued upgrade does not reinstall selected version");
    validate_binary_replacement_eligibility(false, "0.370.0", Some(identity("0.371.0")))
        .expect_err("queued upgrade does not downgrade a newer controller");
    validate_binary_replacement_eligibility(false, "0.370.0", None)
        .expect_err("missing target identity is not eligible");
    validate_binary_replacement_eligibility(true, "0.370.0", Some(identity("0.371.0")))
        .expect("explicit replacement preserves deliberate downgrade semantics");
}

#[cfg(unix)]
#[test]
fn successful_same_version_noop_installer_does_not_prove_replacement() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("target directory");
    let target = directory.path().join("homeboy");
    std::fs::write(&target, "#!/bin/sh\necho 'homeboy 0.370.0'\n")
        .expect("write installed fixture");
    let mut permissions = std::fs::metadata(&target)
        .expect("target metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&target, permissions).expect("make fixture executable");
    let checkpoint = ReplacementCheckpoint::pending(&target, Some("0.370.0"), None, None)
        .expect("capture replacement baseline");

    // Model a successful installer that exits without swapping the target.
    assert!(replacement_applied_identity(&checkpoint)
        .expect("inspect unchanged target")
        .is_none());
    assert_eq!(checkpoint.with_state("not_applied").state, "not_applied");
}

#[cfg(unix)]
#[test]
fn exact_source_bytes_prove_replacement_even_when_identity_probe_fails() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("target directory");
    let target = directory.path().join("homeboy");
    let candidate = directory.path().join("candidate");
    std::fs::write(&target, "#!/bin/sh\necho 'homeboy 0.370.0'\n# old\n")
        .expect("write installed fixture");
    let mut permissions = std::fs::metadata(&target)
        .expect("target metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&target, permissions).expect("make fixture executable");
    std::fs::write(&candidate, b"selected source bytes that cannot execute")
        .expect("write candidate bytes");
    let checkpoint = ReplacementCheckpoint::pending(
        &target,
        Some("0.370.0"),
        None,
        Some(sha256_file(&candidate).expect("hash candidate")),
    )
    .expect("capture replacement baseline");

    std::fs::copy(&candidate, &target).expect("replace target bytes");

    assert!(replacement_was_applied(&checkpoint).expect("inspect exact source bytes"));
    assert!(replacement_applied_identity(&checkpoint).is_err());
}

#[cfg(unix)]
fn write_executable_fixture(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, body).expect("write executable fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make fixture executable");
}

/// #15733: `homeboy upgrade --force` with the selected release already
/// installed staged byte-identical bytes, so the destination digest never
/// changed and the upgrade failed with `did not activate the selected release
/// 0.417.15 ... (observed 0.417.15)`. Unchanged bytes that equal the staged
/// artifact's digest are the selected release, not a failed activation.
#[cfg(unix)]
#[test]
fn already_active_exact_release_bytes_prove_replacement() {
    let directory = tempfile::tempdir().expect("target directory");
    let target = directory.path().join("homeboy");
    let staged = directory.path().join("stage-homeboy");
    let release = "#!/bin/sh\necho 'homeboy 0.417.15+708e00a1d439e7b47141132b80ef5a0894656e83'\n";
    write_executable_fixture(&target, release);
    write_executable_fixture(&staged, release);
    let checkpoint = ReplacementCheckpoint::pending(
        &target,
        Some("0.417.15"),
        None,
        Some(sha256_file(&staged).expect("hash staged release")),
    )
    .expect("capture replacement baseline");

    std::fs::copy(&staged, &target).expect("reinstall identical bytes");

    assert!(replacement_was_applied(&checkpoint).expect("inspect identical release bytes"));
    assert_eq!(
        replacement_applied_identity(&checkpoint)
            .expect("read installed identity")
            .map(|identity| identity.version),
        Some("0.417.15".to_string())
    );
    assert_eq!(
        replacement_observed_state(&checkpoint).expect("observe state"),
        "applied"
    );
}

/// The digest exemption is exact: unchanged bytes that differ from the staged
/// artifact (an older or different build left in place) still fail.
#[cfg(unix)]
#[test]
fn unchanged_bytes_that_differ_from_the_staged_release_are_not_applied() {
    let directory = tempfile::tempdir().expect("target directory");
    let target = directory.path().join("homeboy");
    let staged = directory.path().join("stage-homeboy");
    write_executable_fixture(&target, "#!/bin/sh\necho 'homeboy 0.417.15+aaaaaaa'\n");
    write_executable_fixture(&staged, "#!/bin/sh\necho 'homeboy 0.417.15+bbbbbbb'\n");
    let checkpoint = ReplacementCheckpoint::pending(
        &target,
        Some("0.417.15"),
        None,
        Some(sha256_file(&staged).expect("hash staged release")),
    )
    .expect("capture replacement baseline");

    assert!(!replacement_was_applied(&checkpoint).expect("inspect unchanged target"));
    assert_eq!(
        replacement_observed_state(&checkpoint).expect("observe state"),
        "not_applied"
    );
}

/// End to end over the release promotion step: a forced reinstall of the
/// already-active release stages identical bytes and must succeed.
#[cfg(unix)]
#[test]
fn forced_reinstall_of_the_active_release_is_activated() {
    let directory = tempfile::tempdir().expect("target directory");
    let destination = directory.path().join("homeboy");
    let staged = directory.path().join("stage").join("homeboy");
    std::fs::create_dir_all(staged.parent().unwrap()).expect("stage dir");
    let release = "#!/bin/sh\necho 'homeboy 0.417.15+708e00a1d439e7b47141132b80ef5a0894656e83'\n";
    write_executable_fixture(&destination, release);
    write_executable_fixture(&staged, release);
    let checkpoint = ReplacementCheckpoint::pending(&destination, Some("0.417.15"), None, None)
        .expect("capture replacement baseline");
    let mut states = Vec::new();

    finalize_release_replacement(
        &checkpoint,
        &staged,
        &destination,
        "0.417.15",
        &mut |checkpoint| {
            states.push(checkpoint.state.clone());
            Ok(())
        },
    )
    .expect("already-active exact release activates");

    assert_eq!(states.last().map(String::as_str), Some("applied"));
}

/// A real upgrade still replaces the destination and verifies the staged bytes.
#[cfg(unix)]
#[test]
fn staged_newer_release_replaces_the_destination() {
    let directory = tempfile::tempdir().expect("target directory");
    let destination = directory.path().join("homeboy");
    let staged = directory.path().join("stage").join("homeboy");
    std::fs::create_dir_all(staged.parent().unwrap()).expect("stage dir");
    write_executable_fixture(&destination, "#!/bin/sh\necho 'homeboy 0.417.14+aaaaaaa'\n");
    write_executable_fixture(&staged, "#!/bin/sh\necho 'homeboy 0.417.15+bbbbbbb'\n");
    let checkpoint = ReplacementCheckpoint::pending(&destination, Some("0.417.15"), None, None)
        .expect("capture replacement baseline");

    finalize_release_replacement(&checkpoint, &staged, &destination, "0.417.15", &mut |_| {
        Ok(())
    })
    .expect("newer staged release activates");

    assert_eq!(
        std::fs::read(&destination).unwrap(),
        std::fs::read(&staged).unwrap()
    );
}

/// #11152 stays fixed: a successful installer that staged nothing resolves
/// its candidate to the destination itself, so there is no digest evidence
/// and an unchanged older controller still fails loudly, naming the observed
/// identity rather than silently exiting 0.
#[cfg(unix)]
#[test]
fn no_op_installer_without_a_staged_artifact_still_fails() {
    let directory = tempfile::tempdir().expect("target directory");
    let destination = directory.path().join("homeboy");
    write_executable_fixture(
        &destination,
        "#!/bin/sh\necho 'homeboy 0.326.0+dff9eb75eaf6'\n",
    );
    let checkpoint = ReplacementCheckpoint::pending(&destination, Some("0.326.1"), None, None)
        .expect("capture replacement baseline");
    let mut states = Vec::new();

    let error = finalize_release_replacement(
        &checkpoint,
        &destination,
        &destination,
        "0.326.1",
        &mut |checkpoint| {
            states.push(checkpoint.state.clone());
            Ok(())
        },
    )
    .expect_err("no-op installer must not report activation");

    assert!(error
        .message
        .contains("did not activate the selected release 0.326.1"));
    assert!(error.message.contains("observed 0.326.0"));
    assert_eq!(
        error.details["observed_build_identity"],
        "homeboy 0.326.0+dff9eb75eaf6"
    );
    assert_eq!(states.last().map(String::as_str), Some("not_applied"));
}

/// Even a same-version no-op without a staged artifact fails, but the error
/// no longer reads as the self-contradictory `selected X (observed X)`.
#[cfg(unix)]
#[test]
fn same_version_failure_names_the_byte_discrepancy() {
    let directory = tempfile::tempdir().expect("target directory");
    let destination = directory.path().join("homeboy");
    write_executable_fixture(&destination, "#!/bin/sh\necho 'homeboy 0.417.15+aaaaaaa'\n");
    let checkpoint = ReplacementCheckpoint::pending(&destination, Some("0.417.15"), None, None)
        .expect("capture replacement baseline");

    let error = finalize_release_replacement(
        &checkpoint,
        &destination,
        &destination,
        "0.417.15",
        &mut |_| Ok(()),
    )
    .expect_err("no staged artifact means no activation proof");

    assert!(
        error
            .message
            .contains("bytes were not proven to be the staged release artifact"),
        "{}",
        error.message
    );
    assert!(error.message.contains("homeboy 0.417.15+aaaaaaa"));
}

pub(super) fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
