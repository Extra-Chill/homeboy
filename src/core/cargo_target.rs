use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::paths;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";
pub(crate) const HOMEBOY_CARGO_TARGET_DIR_ENV: &str = "HOMEBOY_CARGO_TARGET_DIR";

/// Build Homeboy's default Cargo target env for Rust component commands.
///
/// The directory is keyed by stable component/repository identity instead of
/// the checkout path, so primary checkouts and Homeboy-managed worktrees share
/// one Cargo target cache. User-provided `CARGO_TARGET_DIR` always wins.
pub(crate) fn env_vars(
    component: &Component,
    source_path: &Path,
    existing_env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    if cargo_target_dir_is_set(existing_env) || !source_path.join("Cargo.toml").exists() {
        return Ok(Vec::new());
    }

    let target_dir = target_dir(component, source_path)?;
    let target_dir = target_dir.to_string_lossy().to_string();
    Ok(vec![
        (CARGO_TARGET_DIR_ENV.to_string(), target_dir.clone()),
        (HOMEBOY_CARGO_TARGET_DIR_ENV.to_string(), target_dir),
    ])
}

pub(crate) fn target_dir(component: &Component, source_path: &Path) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("cargo-targets")
        .join(identity_segment(component, source_path)?))
}

fn cargo_target_dir_is_set(existing_env: &[(String, String)]) -> bool {
    std::env::var_os(CARGO_TARGET_DIR_ENV).is_some()
        || existing_env
            .iter()
            .any(|(key, _)| key == CARGO_TARGET_DIR_ENV)
}

fn identity_segment(component: &Component, source_path: &Path) -> Result<String> {
    let identity = repository_identity(component, source_path)?;
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let label = paths::sanitize_path_segment(&component.id)
        .trim_matches('_')
        .to_string();
    let label = if label.is_empty() {
        "component".to_string()
    } else {
        label
    };
    Ok(format!("{}-{}", label, &hash[..12]))
}

fn repository_identity(component: &Component, source_path: &Path) -> Result<String> {
    if let Some(remote_url) = non_empty(component.remote_url.as_deref()) {
        return Ok(normalize_repo_identity(remote_url));
    }

    if let Some(origin) = git_origin_url(source_path)? {
        return Ok(normalize_repo_identity(&origin));
    }

    Ok(format!("component:{}", component.id))
}

fn git_origin_url(source_path: &Path) -> Result<Option<String>> {
    if !source_path.exists() {
        return Ok(None);
    }

    let output = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(source_path)
        .output()
        .map_err(|e| {
            Error::internal_io(e.to_string(), Some("git remote.origin.url".to_string()))
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(non_empty(Some(&value)).map(ToOwned::to_owned))
}

fn normalize_repo_identity(value: &str) -> String {
    let mut value = value.trim().trim_end_matches('/').to_ascii_lowercase();
    if let Some(rest) = value.strip_prefix("git@github.com:") {
        value = format!("https://github.com/{rest}");
    } else if let Some(rest) = value.strip_prefix("git@github.a8c.com:") {
        value = format!("https://github.a8c.com/{rest}");
    }
    value.strip_suffix(".git").unwrap_or(&value).to_string()
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_git_remote_forms() {
        assert_eq!(
            normalize_repo_identity("git@github.com:Extra-Chill/homeboy.git"),
            "https://github.com/extra-chill/homeboy"
        );
        assert_eq!(
            normalize_repo_identity("https://github.com/Extra-Chill/homeboy.git/"),
            "https://github.com/extra-chill/homeboy"
        );
    }
}
