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
