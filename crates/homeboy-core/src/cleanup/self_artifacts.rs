//! Detection of Homeboy's own temporary build artifacts.
//!
//! These helpers identify detached temp targets, partial temp checkouts, and
//! full source checkouts that belong to Homeboy itself, so `homeboy cleanup`
//! can reclaim space from its own scratch directories without touching
//! unrelated worktrees. Split out of the cleanup command root to keep the
//! parent module under its structural item threshold.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{git, Error, Result};

use super::{
    git_safety, has_tracked_changes_under, path_usage, ArtifactCleanupCandidate,
    ArtifactCleanupOptions, SELF_TEMP_ARTIFACT_CATEGORY, SELF_TEMP_ARTIFACT_LIVENESS,
    SELF_TEMP_ARTIFACT_READINESS,
};

pub(super) fn homeboy_source_checkout() -> Result<PathBuf> {
    let executable = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current Homeboy executable".to_string()),
        )
    })?;
    let working_dir = std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current Homeboy working directory".to_string()),
        )
    })?;
    let cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    homeboy_source_checkout_from_binary(
        &executable,
        &crate::build_identity::current(),
        &working_dir,
        cargo_target_dir.as_deref(),
    )
}

/// Resolve the source checkout that owns a source-built Homeboy binary.
///
/// `CARGO_MANIFEST_DIR` is compiled into binaries and may name a discarded CI
/// filesystem, so it is intentionally not a source-resolution input here.
fn homeboy_source_checkout_from_binary(
    binary: &Path,
    identity: &crate::build_identity::BuildIdentity,
    working_dir: &Path,
    cargo_target_dir: Option<&Path>,
) -> Result<PathBuf> {
    let checkout = source_checkout_from_binary_path(binary).or_else(|| {
        source_checkout_from_managed_target(binary, working_dir, cargo_target_dir)
    }).ok_or_else(|| {
        Error::validation_invalid_argument(
            "self_artifacts",
            format!("no managed Homeboy source checkout owns {}", binary.display()),
            None,
            None,
        )
        .with_hint(
            "`--self` requires a source-built binary at <checkout>/target/{debug,release}/homeboy. Pass an explicit checkout with `homeboy cleanup artifacts --path <PATH>` for an installed binary.",
        )
    })?;
    let checkout = validate_homeboy_manifest_dir(&checkout)?;
    validate_homeboy_build_identity(&checkout, identity)?;
    Ok(checkout)
}

/// A shared Cargo target is intentionally outside its workspace. When Cargo
/// names that target explicitly, the invoking checkout is the only local
/// source candidate and must still prove the binary's recorded revision.
fn source_checkout_from_managed_target(
    binary: &Path,
    working_dir: &Path,
    cargo_target_dir: Option<&Path>,
) -> Option<PathBuf> {
    let target_dir = cargo_target_dir?;
    let target_dir = fs::canonicalize(target_dir).ok()?;
    let binary = fs::canonicalize(binary).ok()?;
    if !binary.starts_with(&target_dir) {
        return None;
    }
    git::get_git_root(&working_dir.to_string_lossy())
        .ok()
        .map(PathBuf::from)
}

fn source_checkout_from_binary_path(binary: &Path) -> Option<PathBuf> {
    let binary = fs::canonicalize(binary).ok()?;
    let binary_name = binary.file_name()?.to_str()?;
    let expected_name = homeboy_product_identity::PRODUCT_IDENTITY.binary_name;
    if binary_name != expected_name && binary_name != format!("{expected_name}.exe") {
        return None;
    }
    let profile = binary.parent()?;
    if !matches!(profile.file_name()?.to_str()?, "debug" | "release") {
        return None;
    }
    let target = profile.parent()?;
    if target.file_name()?.to_str()? != "target" {
        return None;
    }
    target.parent().map(Path::to_path_buf)
}

fn validate_homeboy_build_identity(
    checkout: &Path,
    identity: &crate::build_identity::BuildIdentity,
) -> Result<()> {
    let Some(expected_commit) = identity.git_commit.as_deref() else {
        return Err(no_managed_source(
            checkout,
            "the running binary has no durable source revision",
        ));
    };
    let actual_commit = git::run_git(checkout, &["rev-parse", "HEAD"], "git revision")
        .map_err(|_| no_managed_source(checkout, "the checkout has no readable source revision"))?;
    if !actual_commit.trim().starts_with(expected_commit) {
        return Err(no_managed_source(
            checkout,
            "the checkout revision does not match the running binary",
        ));
    }
    Ok(())
}

fn no_managed_source(path: &Path, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "self_artifacts",
        format!(
            "no managed Homeboy source checkout: {reason} ({})",
            path.display()
        ),
        None,
        None,
    )
    .with_hint("Pass an explicit checkout with `homeboy cleanup artifacts --path <PATH>`.")
}

pub(super) fn validate_homeboy_manifest_dir(manifest_dir: &Path) -> Result<PathBuf> {
    let cargo_toml = manifest_dir.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            format!("{} does not contain Cargo.toml", manifest_dir.display()),
            None,
            None,
        ));
    }

    if !cargo_manifest_package_is_homeboy(&cargo_toml)? {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            format!("{} is not the Homeboy crate manifest", cargo_toml.display()),
            None,
            None,
        ));
    }

    if !is_canonical_git_root(manifest_dir) {
        let mut error = Error::validation_invalid_argument(
            "self_artifacts",
            format!(
                "{} is not a Homeboy source git checkout: not the canonical root of its Git checkout",
                manifest_dir.display()
            ),
            None,
            None,
        )
        .with_hint("`homeboy cleanup artifacts --self` requires a source checkout, not a packaged Cargo registry source.")
        .with_hint("Pass an explicit checkout with `homeboy cleanup artifacts --path <PATH>`, or configure and run from a source checkout.");
        if let Some(checkout) = active_homeboy_checkout_hint() {
            error = error.with_hint(format!(
                "Active Homeboy checkout appears to be: {}",
                checkout.display()
            ));
        }
        return Err(error);
    }

    Ok(manifest_dir.to_path_buf())
}

fn active_homeboy_checkout_hint() -> Option<PathBuf> {
    let owned_checkout = (|| {
        let executable = std::env::current_exe().ok()?;
        let working_dir = std::env::current_dir().ok()?;
        let cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
        let checkout = source_checkout_from_binary_path(&executable).or_else(|| {
            source_checkout_from_managed_target(
                &executable,
                &working_dir,
                cargo_target_dir.as_deref(),
            )
        })?;
        if !checkout.join("Cargo.toml").is_file()
            || !checkout.join("src/main.rs").is_file()
            || !is_canonical_git_root(&checkout)
        {
            return None;
        }
        validate_homeboy_build_identity(&checkout, &crate::build_identity::current()).ok()?;
        Some(checkout)
    })();
    owned_checkout.or_else(|| {
        let working_dir = std::env::current_dir().ok()?;
        working_dir.ancestors().find_map(|candidate| {
            (is_canonical_git_root(candidate)
                && cargo_manifest_package_is_homeboy(&candidate.join("Cargo.toml")).ok()?)
            .then(|| candidate.to_path_buf())
        })
    })
}

fn is_canonical_git_root(path: &Path) -> bool {
    let Ok(root) = git::run_git(path, &["rev-parse", "--show-toplevel"], "git checkout") else {
        return false;
    };
    let (Ok(path), Ok(root)) = (fs::canonicalize(path), fs::canonicalize(root.trim())) else {
        return false;
    };
    path == root
}

pub(super) fn self_temp_artifact_candidates(
    options: &ArtifactCleanupOptions,
) -> Result<Vec<ArtifactCleanupCandidate>> {
    if !options.self_artifacts && options.temp_roots.is_empty() {
        return Ok(Vec::new());
    }

    let roots = if options.temp_roots.is_empty() {
        default_self_temp_roots()
    } else {
        options.temp_roots.clone()
    };
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for root in roots {
        if !root.is_dir() || !seen.insert(root.clone()) {
            continue;
        }
        for entry in fs::read_dir(&root).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read temp root {}", root.display())),
            )
        })? {
            let entry = entry.map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read temp root entry {}", root.display())),
                )
            })?;
            let path = entry.path();
            if !is_detached_homeboy_temp_artifact(&path) {
                if let Some(candidate) = temp_homeboy_checkout_target_candidate(&path)? {
                    candidates.push(candidate);
                } else if let Some(candidate) = partial_homeboy_temp_target_candidate(&path)? {
                    candidates.push(candidate);
                }
                continue;
            }
            let usage = path_usage(&path)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            candidates.push(ArtifactCleanupCandidate {
                worktree: root.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                relative_path: name,
                kind: "detached_homeboy_temp_artifact".to_string(),
                declared_by: "self_temp_root".to_string(),
                category: SELF_TEMP_ARTIFACT_CATEGORY.to_string(),
                size_bytes: usage.logical_bytes,
                allocated_bytes: usage.allocated_bytes,
                age_seconds: usage.age_seconds(),
                liveness: SELF_TEMP_ARTIFACT_LIVENESS.to_string(),
                readiness: SELF_TEMP_ARTIFACT_READINESS.to_string(),
                rehydrate_command: None,
                source_dirty: false,
                unpushed_commits: false,
                pressure_eligible: false,
            });
        }
    }

    Ok(candidates)
}

fn temp_homeboy_checkout_target_candidate(
    checkout: &Path,
) -> Result<Option<ArtifactCleanupCandidate>> {
    if !is_homeboy_source_checkout(checkout)? {
        return Ok(None);
    }

    let target = checkout.join("target");
    let Ok(metadata) = fs::symlink_metadata(&target) else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }

    let safety = match git_safety(checkout) {
        Ok(safety) => safety,
        Err(_) => return Ok(None),
    };
    if has_tracked_changes_under(&safety.dirty_paths, "target") {
        return Ok(None);
    }

    let usage = path_usage(&target)?;
    Ok(Some(ArtifactCleanupCandidate {
        worktree: checkout.to_string_lossy().to_string(),
        path: target.to_string_lossy().to_string(),
        relative_path: "target".to_string(),
        kind: "temp_homeboy_checkout_target".to_string(),
        declared_by: "self_temp_root".to_string(),
        category: SELF_TEMP_ARTIFACT_CATEGORY.to_string(),
        size_bytes: usage.logical_bytes,
        allocated_bytes: usage.allocated_bytes,
        age_seconds: usage.age_seconds(),
        liveness: SELF_TEMP_ARTIFACT_LIVENESS.to_string(),
        readiness: SELF_TEMP_ARTIFACT_READINESS.to_string(),
        rehydrate_command: None,
        source_dirty: safety.source_dirty,
        unpushed_commits: safety.unpushed_commits,
        pressure_eligible: false,
    }))
}

fn partial_homeboy_temp_target_candidate(
    temp_dir: &Path,
) -> Result<Option<ArtifactCleanupCandidate>> {
    let Ok(metadata) = fs::symlink_metadata(temp_dir) else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }

    let Some(name) = temp_dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !name.starts_with("homeboy-")
        || temp_dir.join(".git").exists()
        || temp_dir.join("Cargo.toml").exists()
    {
        return Ok(None);
    }

    let target = temp_dir.join("target");
    let Ok(target_metadata) = fs::symlink_metadata(&target) else {
        return Ok(None);
    };
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if !partial_homeboy_temp_skeleton_is_safe(temp_dir)? {
        return Ok(None);
    }

    let usage = path_usage(&target)?;
    Ok(Some(ArtifactCleanupCandidate {
        worktree: temp_dir.to_string_lossy().to_string(),
        path: target.to_string_lossy().to_string(),
        relative_path: "target".to_string(),
        kind: "partial_homeboy_temp_target".to_string(),
        declared_by: "self_temp_root".to_string(),
        category: SELF_TEMP_ARTIFACT_CATEGORY.to_string(),
        size_bytes: usage.logical_bytes,
        allocated_bytes: usage.allocated_bytes,
        age_seconds: usage.age_seconds(),
        liveness: SELF_TEMP_ARTIFACT_LIVENESS.to_string(),
        readiness: SELF_TEMP_ARTIFACT_READINESS.to_string(),
        rehydrate_command: None,
        source_dirty: false,
        unpushed_commits: false,
        pressure_eligible: false,
    }))
}

fn partial_homeboy_temp_skeleton_is_safe(temp_dir: &Path) -> Result<bool> {
    let mut saw_target = false;
    for entry in fs::read_dir(temp_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read partial temp dir {}", temp_dir.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read partial temp dir entry {}",
                    temp_dir.display()
                )),
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        match name {
            "target" => saw_target = true,
            ".github" | "docs" | "src" | "tests" => {
                if !directory_tree_has_no_files(&entry.path())? {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(saw_target)
}

fn directory_tree_has_no_files(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    for entry in fs::read_dir(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read directory {}", path.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read directory entry {}", path.display())),
            )
        })?;
        let entry_path = entry.path();
        let entry_metadata = fs::symlink_metadata(&entry_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("stat {}", entry_path.display())),
            )
        })?;
        if !entry_metadata.is_dir() || entry_metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if !directory_tree_has_no_files(&entry_path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_homeboy_source_checkout(path: &Path) -> Result<bool> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    if !path.join(".git").exists() || !path.join("Cargo.toml").is_file() {
        return Ok(false);
    }
    if !cargo_manifest_package_is_homeboy(&path.join("Cargo.toml"))? {
        return Ok(false);
    }

    let remotes = match git::run_git(path, &["remote", "-v"], "git remote -v") {
        Ok(output) => output,
        Err(_) => return Ok(false),
    };
    Ok(remotes.lines().any(|line| {
        line.contains("Extra-Chill/homeboy.git") || line.contains("Extra-Chill/homeboy ")
    }))
}

fn cargo_manifest_package_is_homeboy(cargo_toml: &Path) -> Result<bool> {
    let raw = fs::read_to_string(cargo_toml).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", cargo_toml.display())),
        )
    })?;

    let mut in_package = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed == "name = \"homeboy\"" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn default_self_temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![std::env::temp_dir()];
    if let Ok(raw) = std::env::var("HOMEBOY_TRANSIENT_WORKSPACE_ROOTS") {
        roots.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        );
    }
    roots
}

fn is_detached_homeboy_temp_artifact(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    if path.join(".git").exists() || path.join("Cargo.toml").exists() {
        return false;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with("homeboy-")
        && (name.ends_with("-target") || name.contains("-target-") || name.ends_with("-build"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_identity::BuildIdentity;
    use tempfile::TempDir;

    #[test]
    fn resolves_the_canonical_checkout_that_owns_a_source_built_binary() {
        let fixture = TempDir::new().expect("fixture root");
        let checkout = homeboy_checkout(fixture.path(), "source");
        let binary = source_binary(&checkout);
        let identity = build_identity(&checkout);

        let resolved = homeboy_source_checkout_from_binary(&binary, &identity, &checkout, None)
            .expect("source-built binary should resolve its checkout");

        assert_eq!(
            resolved,
            fs::canonicalize(checkout).expect("canonical checkout")
        );
    }

    #[test]
    fn moved_or_installed_binary_does_not_fall_back_to_a_build_machine_path() {
        let fixture = TempDir::new().expect("fixture root");
        let checkout = homeboy_checkout(fixture.path(), "ci-homeboy");
        let source_binary = source_binary(&checkout);
        let moved_binary = fixture.path().join("installed/bin/homeboy");
        fs::create_dir_all(moved_binary.parent().expect("binary parent")).expect("binary parent");
        fs::copy(source_binary, &moved_binary).expect("copy installed binary");

        let error = homeboy_source_checkout_from_binary(
            &moved_binary,
            &build_identity(&checkout),
            fixture.path(),
            None,
        )
        .expect_err("moved binary has no source-layout attachment");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error
            .message
            .contains("no managed Homeboy source checkout owns"));
        assert!(!error.message.contains(&checkout.display().to_string()));
    }

    #[test]
    fn rejects_an_unrelated_homeboy_like_checkout_with_a_different_revision() {
        let fixture = TempDir::new().expect("fixture root");
        let expected = homeboy_checkout(fixture.path(), "expected");
        let unrelated = homeboy_checkout(fixture.path(), "unrelated");

        let error = homeboy_source_checkout_from_binary(
            &source_binary(&unrelated),
            &build_identity(&expected),
            &unrelated,
            None,
        )
        .expect_err("unrelated checkout must not be selected");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("checkout revision does not match"));
    }

    #[test]
    fn rejects_a_source_layout_when_the_binary_has_no_durable_revision() {
        let fixture = TempDir::new().expect("fixture root");
        let checkout = homeboy_checkout(fixture.path(), "source");
        let mut identity = build_identity(&checkout);
        identity.git_commit = None;

        let error = homeboy_source_checkout_from_binary(
            &source_binary(&checkout),
            &identity,
            &checkout,
            None,
        )
        .expect_err("a revisionless binary must not guess its source checkout");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("no durable source revision"));
    }

    #[test]
    fn rejects_a_nested_vendor_manifest_that_is_not_a_canonical_git_root() {
        let fixture = TempDir::new().expect("fixture root");
        let outer = fixture.path().join("outer");
        let vendor = outer.join("vendor/homeboy");
        fs::create_dir_all(vendor.join("src")).expect("vendor directories");
        fs::write(
            vendor.join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        fs::write(vendor.join("src/main.rs"), "fn main() {}\n").expect("source");
        let binary = source_binary(&vendor);
        commit_git_repository(&outer);

        let error =
            homeboy_source_checkout_from_binary(&binary, &build_identity(&outer), &outer, None)
                .expect_err("nested vendor manifest must not be accepted");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("canonical root"));
    }

    fn homeboy_checkout(parent: &Path, name: &str) -> PathBuf {
        let checkout = parent.join(name);
        fs::create_dir_all(checkout.join("src")).expect("checkout directories");
        fs::write(
            checkout.join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        fs::write(checkout.join("src/main.rs"), "fn main() {}\n").expect("source");
        fs::write(checkout.join(".homeboy-fixture"), name).expect("fixture identity");
        commit_git_repository(&checkout);
        checkout
    }

    #[test]
    fn managed_cargo_target_resolves_the_invoking_matching_checkout() {
        let fixture = TempDir::new().expect("fixture root");
        let checkout = homeboy_checkout(fixture.path(), "source");
        let target = fixture.path().join("cargo-target");
        let binary = target.join("debug/deps/homeboy_core-test");
        fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary parent");
        fs::write(&binary, "test binary").expect("test binary");

        let resolved = homeboy_source_checkout_from_binary(
            &binary,
            &build_identity(&checkout),
            &checkout,
            Some(&target),
        )
        .expect("managed target should resolve its invoking checkout");

        assert_eq!(
            resolved,
            fs::canonicalize(checkout).expect("canonical checkout")
        );
    }

    fn source_binary(checkout: &Path) -> PathBuf {
        let binary = checkout.join("target/release/homeboy");
        fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary parent");
        fs::write(&binary, "binary").expect("binary");
        binary
    }

    fn build_identity(checkout: &Path) -> BuildIdentity {
        let commit = git_output(checkout, &["rev-parse", "--short=12", "HEAD"]);
        BuildIdentity {
            version: "0.1.0".to_string(),
            git_commit: Some(commit.clone()),
            git_dirty: Some(false),
            display: format!("homeboy 0.1.0+{commit}"),
        }
    }

    fn commit_git_repository(path: &Path) {
        run_git(path, &["init", "-q"]);
        run_git(path, &["add", "."]);
        run_git(
            path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-qm",
                "initial",
            ],
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {} failed", args.join(" "));
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_string()
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
