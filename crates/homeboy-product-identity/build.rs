use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/source_upgrade_provenance.rs"]
mod source_upgrade_provenance;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("product identity must be nested below the workspace root");
    let manifest = root.join("Cargo.toml");

    println!("cargo:rerun-if-changed={}", manifest.display());
    println!(
        "cargo:rustc-env=HOMEBOY_PRODUCT_VERSION={}",
        root_package_version(&manifest)
    );
    emit_git_identity(root);
}

fn root_package_version(manifest: &Path) -> String {
    let manifest = fs::read_to_string(manifest).expect("read root Cargo.toml");
    let package = manifest
        .split("[package]")
        .nth(1)
        .expect("root Cargo.toml must contain [package]");
    package
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = "))
        .and_then(|value| value.trim_matches('"').split('"').next())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .expect("root Cargo.toml [package] must contain a version")
}

fn emit_git_identity(root: &Path) {
    println!(
        "cargo:rerun-if-env-changed={}",
        source_upgrade_provenance::GIT_COMMIT_ENV
    );
    println!(
        "cargo:rerun-if-env-changed={}",
        source_upgrade_provenance::GIT_DIRTY_ENV
    );
    let source_upgrade_commit = env::var(source_upgrade_provenance::GIT_COMMIT_ENV).ok();
    let source_upgrade_dirty = env::var(source_upgrade_provenance::GIT_DIRTY_ENV).ok();
    let source_upgrade_provenance = source_upgrade_provenance::parse_source_upgrade_provenance(
        source_upgrade_commit.as_deref(),
        source_upgrade_dirty.as_deref(),
    )
    .unwrap_or_else(|error| panic!("invalid source-upgrade build provenance: {error}"));
    let git_dir = resolve_git_dir(root).unwrap_or_else(|| root.join(".git"));
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
    if let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }

    if let Some(provenance) = source_upgrade_provenance {
        println!(
            "cargo:rustc-env=HOMEBOY_PRODUCT_GIT_COMMIT={}",
            provenance.git_commit
        );
        println!(
            "cargo:rustc-env=HOMEBOY_PRODUCT_GIT_DIRTY={}",
            provenance.git_dirty
        );
    } else if let Some(commit) = git_output(root, &["rev-parse", "--short=12", "HEAD"]) {
        println!("cargo:rustc-env=HOMEBOY_PRODUCT_GIT_COMMIT={commit}");
    }
    if let Some(status) = git_output(root, &["status", "--porcelain"]) {
        println!(
            "cargo:rustc-env=HOMEBOY_PRODUCT_GIT_DIRTY={}",
            if status.is_empty() { "false" } else { "true" }
        );
    }
}

fn resolve_git_dir(root: &Path) -> Option<PathBuf> {
    let git_path = root.join(".git");
    if git_path.is_dir() {
        return Some(git_path);
    }

    let raw_path = fs::read_to_string(&git_path)
        .ok()?
        .trim()
        .strip_prefix("gitdir: ")?
        .trim()
        .to_string();
    let path = PathBuf::from(raw_path);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
