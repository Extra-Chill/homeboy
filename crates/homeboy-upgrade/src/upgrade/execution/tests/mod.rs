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
    let package_manifest = ["Car", "go.toml"].concat();
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
