use crate::core::build_identity::{self, BuildIdentity};
use crate::core::upgrade::{self, InstallMethod};
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
    pub active_build_identity: BuildIdentity,
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
    let active_build_identity = build_identity::current();
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
        active_build_identity,
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
    let secondary_bin = format!(
        "/.{}/bin/",
        crate::core::defaults::secondary_install_method_key()
    );
    if raw.contains(&secondary_bin) {
        return InstallMethod::Secondary;
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
        let package_manifest = ["Car", "go.toml"].concat();
        if ancestor.join(package_manifest).is_file() && ancestor.join("src/main.rs").is_file() {
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

// ----------------------------------------------------------------------------
// Authoritative runtime view (#4861)
// ----------------------------------------------------------------------------

/// One controller-side input describing the binary that is actually executing
/// the current `homeboy` invocation. Kept free of probes so the assembler is a
/// pure function over already-collected facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerRuntimeInput {
    pub binary_path: Option<String>,
    pub version: String,
    pub build_identity: BuildIdentity,
    pub install_method: InstallMethod,
    /// Optional source checkout backing the active binary (debug builds).
    pub source_checkout: Option<SourceCheckoutStatus>,
}

/// One runner-side input describing a configured runner and, when connected,
/// the binary identity of its active daemon. All fields are pre-resolved facts
/// so the assembler stays deterministic and unit-testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerRuntimeInput {
    pub runner_id: String,
    pub kind: String,
    #[allow(clippy::struct_field_names)]
    pub server_id: Option<String>,
    /// Configured runner executable path (`settings.homeboy_path`).
    pub configured_binary_path: Option<String>,
    pub connected: bool,
    /// Version reported by the active daemon session, when connected.
    pub daemon_version: Option<String>,
    /// Build identity reported by the active daemon session, when connected.
    pub daemon_build_identity: Option<String>,
    /// True when the active daemon was started by a build that differs from the
    /// configured runner executable (existing stale-daemon drift signal).
    pub daemon_drift: bool,
}

/// Authoritative identity of a single runtime participant (controller or
/// runner) collapsed to the fields operators reason about when deciding which
/// binary to use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeParticipant {
    /// `controller` or `runner`.
    pub role: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    /// Authoritative version string for this participant, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Authoritative build-identity display string, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<String>,
    /// The binary path operators should treat as authoritative for this
    /// participant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_method: Option<InstallMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_checkout: Option<SourceCheckoutStatus>,
    pub connected: bool,
    /// Relation of this participant's version to the controller's version.
    pub relation_to_controller: VersionRelation,
    /// True when this participant's active runtime disagrees with its own
    /// configured/authoritative binary (e.g. a stale daemon).
    pub internal_drift: bool,
}

/// One authoritative runtime view spanning the controller and every configured
/// runner, plus the drift signals an operator needs to decide whether a single
/// repair is required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeView {
    pub command: String,
    /// The controller is always the authoritative reference point.
    pub controller: RuntimeParticipant,
    pub runners: Vec<RuntimeParticipant>,
    /// True when any participant disagrees with the controller version or has
    /// internal drift. When false, operators can trust a single binary view.
    pub agrees: bool,
    /// Human-facing, deterministic notes describing each disagreement.
    pub drift_notes: Vec<String>,
}

/// Pure assembler: collapse already-collected controller and runner inputs into
/// one authoritative runtime view. Deterministic and side-effect free so it can
/// be unit tested without touching the registry, SSH, or daemons.
pub fn build_runtime_view(
    controller: ControllerRuntimeInput,
    runners: Vec<RunnerRuntimeInput>,
) -> RuntimeView {
    let controller_version = controller.version.clone();

    let controller_participant = RuntimeParticipant {
        role: "controller".to_string(),
        id: "controller".to_string(),
        kind: None,
        server_id: None,
        version: Some(controller.version.clone()),
        build_identity: Some(controller.build_identity.display.clone()),
        binary_path: controller.binary_path.clone(),
        install_method: Some(controller.install_method),
        source_checkout: controller.source_checkout.clone(),
        connected: true,
        relation_to_controller: VersionRelation::Current,
        internal_drift: false,
    };

    let mut drift_notes = Vec::new();
    let mut runner_participants = Vec::with_capacity(runners.len());

    for runner in runners {
        let relation = match runner.daemon_version.as_deref() {
            Some(version) => relation_to_latest(version, Some(&controller_version)),
            None => VersionRelation::Unknown,
        };

        if runner.connected {
            if let Some(version) = runner.daemon_version.as_deref() {
                if relation != VersionRelation::Current {
                    drift_notes.push(format!(
                        "runner `{}` active daemon is {version} ({}) but controller is {controller_version}",
                        runner.runner_id,
                        relation_label(&relation),
                    ));
                }
            }
        }

        if runner.daemon_drift {
            drift_notes.push(format!(
                "runner `{}` active daemon was started by a different build than its configured executable",
                runner.runner_id
            ));
        }

        runner_participants.push(RuntimeParticipant {
            role: "runner".to_string(),
            id: runner.runner_id,
            kind: Some(runner.kind),
            server_id: runner.server_id,
            version: runner.daemon_version,
            build_identity: runner.daemon_build_identity,
            binary_path: runner.configured_binary_path,
            install_method: None,
            source_checkout: None,
            connected: runner.connected,
            relation_to_controller: relation,
            internal_drift: runner.daemon_drift,
        });
    }

    RuntimeView {
        command: "self doctor".to_string(),
        controller: controller_participant,
        runners: runner_participants,
        agrees: drift_notes.is_empty(),
        drift_notes,
    }
}

fn relation_label(relation: &VersionRelation) -> &'static str {
    match relation {
        VersionRelation::Current => "current",
        VersionRelation::Behind => "behind",
        VersionRelation::Ahead => "ahead",
        VersionRelation::Unknown => "unknown",
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
        assert_eq!(
            status.active_build_identity.version,
            upgrade::current_version()
        );
        assert!(status.active_build_identity.display.starts_with("homeboy "));
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
            Some(PathBuf::from(format!(
                "/Users/test/.{}/bin/homeboy",
                crate::core::defaults::secondary_install_method_key()
            ))),
            || Err("offline".to_string()),
            |_cmd, _args| Err("not installed".to_string()),
        );

        assert_eq!(status.install_method, InstallMethod::Secondary);
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
        assert_eq!(
            json["active_build_identity"]["version"],
            upgrade::current_version()
        );
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

    fn identity(version: &str) -> BuildIdentity {
        BuildIdentity {
            version: version.to_string(),
            git_commit: None,
            git_dirty: None,
            display: format!("homeboy {version}"),
        }
    }

    fn controller_input(version: &str) -> ControllerRuntimeInput {
        ControllerRuntimeInput {
            binary_path: Some("/opt/homebrew/bin/homeboy".to_string()),
            version: version.to_string(),
            build_identity: identity(version),
            install_method: InstallMethod::Homebrew,
            source_checkout: None,
        }
    }

    #[test]
    fn runtime_view_agrees_when_runners_match_controller() {
        let view = build_runtime_view(
            controller_input("0.235.0"),
            vec![RunnerRuntimeInput {
                runner_id: "lab".to_string(),
                kind: "ssh".to_string(),
                server_id: Some("lab-host".to_string()),
                configured_binary_path: Some("/home/runner/target/release/homeboy".to_string()),
                connected: true,
                daemon_version: Some("0.235.0".to_string()),
                daemon_build_identity: Some("homeboy 0.235.0".to_string()),
                daemon_drift: false,
            }],
        );

        assert_eq!(view.command, "self doctor");
        assert_eq!(view.controller.role, "controller");
        assert_eq!(view.controller.version.as_deref(), Some("0.235.0"));
        assert_eq!(
            view.controller.relation_to_controller,
            VersionRelation::Current
        );
        assert_eq!(view.runners.len(), 1);
        let runner = &view.runners[0];
        assert_eq!(runner.role, "runner");
        assert_eq!(runner.id, "lab");
        assert_eq!(runner.relation_to_controller, VersionRelation::Current);
        assert!(runner.connected);
        assert!(view.agrees);
        assert!(view.drift_notes.is_empty());
    }

    #[test]
    fn runtime_view_flags_runner_version_skew() {
        let view = build_runtime_view(
            controller_input("0.235.0"),
            vec![RunnerRuntimeInput {
                runner_id: "lab".to_string(),
                kind: "ssh".to_string(),
                server_id: Some("lab-host".to_string()),
                configured_binary_path: None,
                connected: true,
                daemon_version: Some("0.229.11".to_string()),
                daemon_build_identity: Some("homeboy 0.229.11".to_string()),
                daemon_drift: false,
            }],
        );

        assert!(!view.agrees);
        assert_eq!(
            view.runners[0].relation_to_controller,
            VersionRelation::Behind
        );
        assert_eq!(view.drift_notes.len(), 1);
        assert!(view.drift_notes[0].contains("lab"));
        assert!(view.drift_notes[0].contains("0.229.11"));
        assert!(view.drift_notes[0].contains("0.235.0"));
    }

    #[test]
    fn runtime_view_reports_internal_daemon_drift() {
        let view = build_runtime_view(
            controller_input("0.235.0"),
            vec![RunnerRuntimeInput {
                runner_id: "lab".to_string(),
                kind: "ssh".to_string(),
                server_id: None,
                configured_binary_path: Some("/srv/homeboy".to_string()),
                connected: true,
                daemon_version: Some("0.235.0".to_string()),
                daemon_build_identity: Some("homeboy 0.235.0+deadbeef".to_string()),
                daemon_drift: true,
            }],
        );

        assert!(!view.agrees);
        assert!(view.runners[0].internal_drift);
        assert_eq!(
            view.runners[0].relation_to_controller,
            VersionRelation::Current
        );
        assert_eq!(view.drift_notes.len(), 1);
        assert!(view.drift_notes[0].contains("different build"));
    }

    #[test]
    fn runtime_view_disconnected_runner_is_unknown_not_drift() {
        let view = build_runtime_view(
            controller_input("0.235.0"),
            vec![RunnerRuntimeInput {
                runner_id: "lab".to_string(),
                kind: "ssh".to_string(),
                server_id: None,
                configured_binary_path: Some("/srv/homeboy".to_string()),
                connected: false,
                daemon_version: None,
                daemon_build_identity: None,
                daemon_drift: false,
            }],
        );

        assert!(view.agrees);
        assert!(view.drift_notes.is_empty());
        assert!(!view.runners[0].connected);
        assert_eq!(
            view.runners[0].relation_to_controller,
            VersionRelation::Unknown
        );
    }

    #[test]
    fn runtime_view_json_shape_is_stable() {
        let view = build_runtime_view(controller_input("0.235.0"), Vec::new());
        let json = serde_json::to_value(view).unwrap();

        assert_eq!(json["command"], "self doctor");
        assert_eq!(json["controller"]["role"], "controller");
        assert_eq!(json["controller"]["version"], "0.235.0");
        assert_eq!(json["controller"]["connected"], true);
        assert_eq!(json["agrees"], true);
        assert!(json["runners"].as_array().unwrap().is_empty());
    }
}
