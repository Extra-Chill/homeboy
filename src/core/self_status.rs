use crate::upgrade::{self, InstallMethod};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const BREW_FORMULA: &str = "extra-chill/tap/homeboy";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VersionRelation {
    Current,
    Behind,
    Ahead,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeValue {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomebrewStatus {
    pub formula: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceCheckoutStatus {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfStatus {
    pub command: String,
    pub active_binary: String,
    pub active_version: String,
    pub install_method: InstallMethod,
    pub latest_github_release: ProbeValue,
    pub homebrew: HomebrewStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_checkout: Option<SourceCheckoutStatus>,
    pub version_relation: VersionRelation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn collect_status() -> SelfStatus {
    collect_status_with(
        std::env::current_exe().ok(),
        || upgrade::fetch_latest_version(InstallMethod::Homebrew).map_err(|e| e.to_string()),
        run_external,
    )
}

pub fn collect_status_with<F, R>(
    active_binary: Option<PathBuf>,
    fetch_latest_github: F,
    run: R,
) -> SelfStatus
where
    F: Fn() -> Result<String, String>,
    R: Fn(&str, &[&str]) -> Result<ProbeOutput, String>,
{
    let active_binary_string = active_binary
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let active_version = upgrade::current_version().to_string();
    let install_method = detect_install_method_from_path(active_binary.as_deref());
    let latest_github_release = probe_latest_github(fetch_latest_github);
    let homebrew = probe_homebrew(&run);
    let source_checkout = active_binary
        .as_deref()
        .and_then(|path| probe_source(path, &run));
    let version_relation = relation_to_latest(
        &active_version,
        latest_github_release
            .version
            .as_deref()
            .or(homebrew.stable_version.as_deref()),
    );

    SelfStatus {
        command: "self status".to_string(),
        active_binary: active_binary_string,
        active_version,
        install_method,
        latest_github_release,
        homebrew,
        source_checkout,
        version_relation,
    }
}

fn run_external(command: &str, args: &[&str]) -> Result<ProbeOutput, String> {
    Command::new(command)
        .args(args)
        .output()
        .map(|output| ProbeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
        .map_err(|e| e.to_string())
}

fn detect_install_method_from_path(path: Option<&Path>) -> InstallMethod {
    let Some(path) = path else {
        return InstallMethod::Unknown;
    };
    let raw = path.to_string_lossy();

    if raw.contains("/Homebrew/")
        || raw.contains("/homebrew/")
        || raw.contains("/Cellar/homeboy/")
        || raw.contains("/.linuxbrew/")
    {
        return InstallMethod::Homebrew;
    }
    if raw.contains("/.cargo/bin/") {
        return InstallMethod::Cargo;
    }
    if raw.contains("/target/debug/")
        || raw.contains("/target/release/")
        || raw.contains("homeboy@")
    {
        return InstallMethod::Source;
    }

    InstallMethod::Binary
}

fn probe_latest_github<F>(fetch_latest_github: F) -> ProbeValue
where
    F: Fn() -> Result<String, String>,
{
    match fetch_latest_github() {
        Ok(version) => ProbeValue {
            available: true,
            version: Some(version),
            error: None,
        },
        Err(error) => ProbeValue {
            available: false,
            version: None,
            error: Some(error),
        },
    }
}

fn probe_homebrew<R>(run: &R) -> HomebrewStatus
where
    R: Fn(&str, &[&str]) -> Result<ProbeOutput, String>,
{
    match run("brew", &["info", "--json=v2", BREW_FORMULA]) {
        Ok(output) if output.success => match parse_brew_info(&output.stdout) {
            Ok((stable_version, installed_version)) => HomebrewStatus {
                formula: BREW_FORMULA.to_string(),
                available: true,
                stable_version,
                installed_version,
                error: None,
            },
            Err(error) => HomebrewStatus {
                formula: BREW_FORMULA.to_string(),
                available: false,
                stable_version: None,
                installed_version: None,
                error: Some(error),
            },
        },
        Ok(output) => HomebrewStatus {
            formula: BREW_FORMULA.to_string(),
            available: false,
            stable_version: None,
            installed_version: None,
            error: Some(non_empty(output.stderr).unwrap_or_else(|| "brew info failed".to_string())),
        },
        Err(error) => HomebrewStatus {
            formula: BREW_FORMULA.to_string(),
            available: false,
            stable_version: None,
            installed_version: None,
            error: Some(error),
        },
    }
}

fn parse_brew_info(raw: &str) -> Result<(Option<String>, Option<String>), String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| format!("parse brew JSON: {e}"))?;
    let formula = value
        .get("formulae")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| "brew info returned no formulae".to_string())?;

    let stable_version = formula
        .pointer("/versions/stable")
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_string);
    let installed_version = formula
        .get("installed")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("version"))
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_string);

    Ok((stable_version, installed_version))
}

fn probe_source<R>(binary: &Path, run: &R) -> Option<SourceCheckoutStatus>
where
    R: Fn(&str, &[&str]) -> Result<ProbeOutput, String>,
{
    let checkout = find_source_checkout(binary)?;
    let checkout_string = checkout.to_string_lossy().to_string();
    let branch = git_probe(&checkout, &["branch", "--show-current"], run);
    let head = git_probe(&checkout, &["rev-parse", "--short", "HEAD"], run);
    let dirty = match git_probe(&checkout, &["status", "--porcelain"], run) {
        Ok(output) => Some(!output.is_empty()),
        Err(_) => None,
    };

    let error = branch
        .as_ref()
        .err()
        .or_else(|| head.as_ref().err())
        .map(|e| e.to_string());

    Some(SourceCheckoutStatus {
        path: checkout_string,
        branch: branch.ok().and_then(non_empty),
        head: head.ok().and_then(non_empty),
        dirty,
        error,
    })
}

fn find_source_checkout(binary: &Path) -> Option<PathBuf> {
    for ancestor in binary.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some("target") {
            return ancestor.parent().map(Path::to_path_buf);
        }
        if ancestor.join("Cargo.toml").is_file() && ancestor.join("src/main.rs").is_file() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn git_probe<R>(checkout: &Path, args: &[&str], run: &R) -> Result<String, String>
where
    R: Fn(&str, &[&str]) -> Result<ProbeOutput, String>,
{
    let checkout_arg = checkout.to_string_lossy();
    let mut full_args = vec!["-C", checkout_arg.as_ref()];
    full_args.extend_from_slice(args);
    match run("git", &full_args) {
        Ok(output) if output.success => Ok(output.stdout),
        Ok(output) => {
            Err(non_empty(output.stderr).unwrap_or_else(|| "git command failed".to_string()))
        }
        Err(error) => Err(error),
    }
}

fn relation_to_latest(active: &str, latest: Option<&str>) -> VersionRelation {
    let Some(latest) = latest else {
        return VersionRelation::Unknown;
    };

    let active = normalize_version(active);
    let latest = normalize_version(latest);
    match (Version::parse(&active), Version::parse(&latest)) {
        (Ok(active), Ok(latest)) if active == latest => VersionRelation::Current,
        (Ok(active), Ok(latest)) if active < latest => VersionRelation::Behind,
        (Ok(active), Ok(latest)) if active > latest => VersionRelation::Ahead,
        _ => VersionRelation::Unknown,
    }
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn output(stdout: &str) -> ProbeOutput {
        ProbeOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn status_collects_active_binary_versions_and_brew_probe() {
        let mut probes = HashMap::new();
        probes.insert(
            "brew info --json=v2 extra-chill/tap/homeboy".to_string(),
            output(r#"{"formulae":[{"versions":{"stable":"0.114.2"},"installed":[{"version":"0.114.1"}]}]}"#),
        );

        let status = collect_status_with(
            Some(PathBuf::from("/opt/homebrew/bin/homeboy")),
            || Ok("0.114.2".to_string()),
            |cmd, args| {
                probes
                    .get(&format!("{} {}", cmd, args.join(" ")))
                    .cloned()
                    .ok_or_else(|| "missing probe".to_string())
            },
        );

        assert_eq!(status.command, "self status");
        assert_eq!(status.active_binary, "/opt/homebrew/bin/homeboy");
        assert_eq!(status.active_version, upgrade::current_version());
        assert_eq!(status.install_method, InstallMethod::Homebrew);
        assert_eq!(
            status.latest_github_release.version.as_deref(),
            Some("0.114.2")
        );
        assert_eq!(status.homebrew.stable_version.as_deref(), Some("0.114.2"));
        assert_eq!(
            status.homebrew.installed_version.as_deref(),
            Some("0.114.1")
        );
    }

    #[test]
    fn external_probe_failures_do_not_fail_status_collection() {
        let status = collect_status_with(
            Some(PathBuf::from("/Users/test/.cargo/bin/homeboy")),
            || Err("offline".to_string()),
            |_cmd, _args| Err("not installed".to_string()),
        );

        assert_eq!(status.install_method, InstallMethod::Cargo);
        assert!(!status.latest_github_release.available);
        assert_eq!(
            status.latest_github_release.error.as_deref(),
            Some("offline")
        );
        assert!(!status.homebrew.available);
        assert_eq!(status.homebrew.error.as_deref(), Some("not installed"));
        assert_eq!(status.version_relation, VersionRelation::Unknown);
    }

    #[test]
    fn parses_brew_info_json_shape() {
        let (stable, installed) = parse_brew_info(
            r#"{"formulae":[{"versions":{"stable":"0.114.2"},"installed":[{"version":"0.114.1"}]}]}"#,
        )
        .unwrap();

        assert_eq!(stable.as_deref(), Some("0.114.2"));
        assert_eq!(installed.as_deref(), Some("0.114.1"));
    }

    #[test]
    fn compares_versions_with_v_prefixes() {
        assert_eq!(
            relation_to_latest("0.114.1", Some("v0.114.2")),
            VersionRelation::Behind
        );
        assert_eq!(
            relation_to_latest("0.114.2", Some("0.114.2")),
            VersionRelation::Current
        );
        assert_eq!(
            relation_to_latest("0.114.3", Some("0.114.2")),
            VersionRelation::Ahead
        );
        assert_eq!(
            relation_to_latest("0.114.2", None),
            VersionRelation::Unknown
        );
    }

    #[test]
    fn json_shape_keeps_failed_probes_structured() {
        let status = collect_status_with(
            Some(PathBuf::from("/tmp/homeboy")),
            || Err("github unavailable".to_string()),
            |_cmd, _args| Err("brew unavailable".to_string()),
        );
        let json = serde_json::to_value(status).unwrap();

        assert_eq!(json["command"], "self status");
        assert_eq!(json["active_binary"], "/tmp/homeboy");
        assert_eq!(json["latest_github_release"]["available"], false);
        assert_eq!(json["latest_github_release"]["error"], "github unavailable");
        assert_eq!(json["homebrew"]["available"], false);
        assert_eq!(json["homebrew"]["error"], "brew unavailable");
        assert_eq!(json["version_relation"], "unknown");
    }

    #[test]
    fn test_collect_status() {
        let status = collect_status_with(
            None,
            || Ok(upgrade::current_version().to_string()),
            |_cmd, _args| Err("external probe skipped".to_string()),
        );

        assert_eq!(status.active_binary, "unknown");
        assert_eq!(status.active_version, upgrade::current_version());
        assert_eq!(status.install_method, InstallMethod::Unknown);
        assert_eq!(status.version_relation, VersionRelation::Current);
    }
}
