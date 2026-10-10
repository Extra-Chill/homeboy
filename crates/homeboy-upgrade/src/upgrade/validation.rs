use homeboy_core::error::Result;

use super::helpers::{
    current_version, detect_install_method, fetch_latest_version, version_is_newer,
};
use super::release_catalog::{self, InstallableSelection};
use super::types::{InstallMethod, VersionCheck};

/// Whether this install method resolves a *specific* published release before
/// downloading it. That is the only method for which the check can promise the
/// reported release is the one the upgrade will install.
///
pub(crate) fn selects_an_installable_release(method: InstallMethod) -> bool {
    matches!(method, InstallMethod::Binary | InstallMethod::Secondary)
}

pub fn check_for_updates() -> Result<VersionCheck> {
    let install_method = detect_install_method();
    let current = current_version().to_string();

    if selects_an_installable_release(install_method) {
        if let Some(check) = installable_check(install_method, &current) {
            return Ok(check);
        }
        // The release list was unreachable. Fall through to the single-release
        // fetch rather than reporting "no update": one failing endpoint is not
        // evidence about the release train.
    }

    let latest = fetch_latest_version(install_method).ok();
    let update_available = latest
        .as_ref()
        .map(|l| version_is_newer(l, &current))
        .unwrap_or(false);

    Ok(VersionCheck {
        command: "upgrade.check".to_string(),
        current_version: current,
        latest_version: latest.clone(),
        update_available,
        install_method,
        target: release_catalog::running_target_triple().map(str::to_string),
        newest_version: latest,
        uninstallable_versions: Vec::new(),
        notice: None,
        runners: Vec::new(),
    })
}

/// `upgrade --check` with runner selection: the controller verdict plus a
/// read-only view of each selected runner's selected binary and runner-service
/// binaries against the version that runner would converge to (#15733).
pub fn check_for_updates_with_runners(
    runner_targets: &[String],
    runner_only: bool,
) -> Result<VersionCheck> {
    let mut check = check_for_updates()?;
    if runner_targets.is_empty() {
        return Ok(check);
    }
    let target = runner_check_target_version(&check, runner_only);
    check.runners = super::with_runner_upgrade(|provider| {
        provider.check_configured_runners(runner_targets, &target)
    })?;
    Ok(check)
}

/// Runners converge to the controller identity. `--runner-only` leaves the
/// controller in place, so its current version is the target; otherwise an
/// installable update is what the controller (and then its runners) becomes.
pub(crate) fn runner_check_target_version(check: &VersionCheck, runner_only: bool) -> String {
    if runner_only || !check.update_available {
        return check.current_version.clone();
    }
    check
        .latest_version
        .clone()
        .unwrap_or_else(|| check.current_version.clone())
}

/// Build a check verdict from the release catalog, or `None` when the catalog
/// could not be fetched.
fn installable_check(install_method: InstallMethod, current: &str) -> Option<VersionCheck> {
    let releases = release_catalog::fetch_release_catalog().ok()?;
    let target = release_catalog::running_target_triple();
    let selection = release_catalog::select_installable(&releases, target);

    Some(version_check_from_selection(
        install_method,
        current,
        target,
        &selection,
    ))
}

/// Pure projection of a selection into the reported verdict.
///
/// `update_available` is deliberately keyed off the *installable* release. The
/// bug this fixes is `--check` answering a question the operator did not ask:
/// it reported an update was available for a release this platform could not
/// install, and the upgrade then 404'd (#11750).
pub(crate) fn version_check_from_selection(
    install_method: InstallMethod,
    current: &str,
    target: Option<&str>,
    selection: &InstallableSelection,
) -> VersionCheck {
    let installable = selection
        .installable
        .as_ref()
        .map(|entry| entry.version.clone());
    let newest = selection.newest.as_ref().map(|entry| entry.version.clone());

    let update_available = installable
        .as_ref()
        .map(|version| version_is_newer(version, current))
        .unwrap_or(false);

    let notice = match (target, &installable) {
        (Some(target), Some(_)) => selection.check_fallback_notice(target),
        (Some(target), None) => selection.newest.as_ref().map(|newest| {
            format!(
                "No published release ships a {target} asset (newest release: {}). Upgrade with --method source --source-path <PATH>, or wait for a release that publishes this target.",
                newest.tag
            )
        }),
        (None, _) => Some(format!(
            "The running target triple could not be determined ({}), so release asset availability was not verified.",
            release_catalog::running_platform_description()
        )),
    };

    VersionCheck {
        command: "upgrade.check".to_string(),
        current_version: current.to_string(),
        latest_version: installable,
        update_available,
        install_method,
        target: target.map(str::to_string),
        newest_version: newest,
        uninstallable_versions: selection.skipped_versions(),
        notice,
        runners: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upgrade::release_catalog::ReleaseRef;

    const LINUX: &str = "x86_64-unknown-linux-gnu";

    fn release_ref(version: &str) -> ReleaseRef {
        ReleaseRef {
            tag: format!("v{version}"),
            version: version.to_string(),
        }
    }

    /// The reported defect: `--check` said an update was available for
    /// `v0.333.0`, and `upgrade` then 404'd because that release has no Linux
    /// asset. The verdict must describe the release that can actually be
    /// installed.
    #[test]
    fn check_reports_the_installable_release_not_the_newest_tag() {
        let selection = InstallableSelection {
            newest: Some(release_ref("0.333.0")),
            installable: Some(release_ref("0.332.0")),
            skipped: vec![release_ref("0.333.0")],
        };

        let check =
            version_check_from_selection(InstallMethod::Binary, "0.327.0", Some(LINUX), &selection);

        assert!(check.update_available);
        assert_eq!(check.latest_version.as_deref(), Some("0.332.0"));
        assert_eq!(check.newest_version.as_deref(), Some("0.333.0"));
        assert_eq!(check.uninstallable_versions, vec!["0.333.0".to_string()]);
        assert_eq!(check.target.as_deref(), Some(LINUX));
        assert!(check
            .notice
            .as_deref()
            .expect("fallback is explained")
            .contains("v0.332.0 is the newest installable release"));
    }

    /// The narrowest statement of the bug: when the *only* newer release is
    /// uninstallable here, `--check` must not report an available update. It
    /// would send the operator straight into the 404.
    #[test]
    fn check_refuses_to_report_an_uninstallable_release_as_available() {
        let selection = InstallableSelection {
            newest: Some(release_ref("0.333.0")),
            installable: Some(release_ref("0.327.0")),
            skipped: vec![release_ref("0.333.0")],
        };

        let check =
            version_check_from_selection(InstallMethod::Binary, "0.327.0", Some(LINUX), &selection);

        assert!(!check.update_available);
        assert_eq!(check.latest_version.as_deref(), Some("0.327.0"));
        assert_eq!(check.newest_version.as_deref(), Some("0.333.0"));
    }

    /// No artifact anywhere is not "you are up to date". It is reported as no
    /// installable release, with the newest tag still visible so the operator
    /// can see what they are missing.
    #[test]
    fn check_reports_no_installable_release_without_claiming_currency() {
        let selection = InstallableSelection {
            newest: Some(release_ref("0.333.0")),
            installable: None,
            skipped: Vec::new(),
        };

        let check =
            version_check_from_selection(InstallMethod::Binary, "0.327.0", Some(LINUX), &selection);

        assert!(!check.update_available);
        assert!(check.latest_version.is_none());
        assert_eq!(check.newest_version.as_deref(), Some("0.333.0"));
        assert!(check
            .notice
            .as_deref()
            .expect("absence is explained")
            .contains("No published release ships a x86_64-unknown-linux-gnu asset"));
    }

    /// An undetermined target must say so instead of guessing a triple and
    /// reporting a verified-looking verdict.
    #[test]
    fn check_says_so_when_the_running_target_is_undetermined() {
        let selection = InstallableSelection {
            newest: Some(release_ref("0.333.0")),
            installable: Some(release_ref("0.333.0")),
            skipped: Vec::new(),
        };

        let check =
            version_check_from_selection(InstallMethod::Binary, "0.327.0", None, &selection);

        assert!(check.target.is_none());
        assert!(check.update_available);
        assert!(check
            .notice
            .as_deref()
            .expect("undetermined target is explained")
            .contains("could not be determined"));
    }

    #[test]
    fn healthy_release_train_reports_no_notice() {
        let selection = InstallableSelection {
            newest: Some(release_ref("0.333.0")),
            installable: Some(release_ref("0.333.0")),
            skipped: Vec::new(),
        };

        let check =
            version_check_from_selection(InstallMethod::Binary, "0.327.0", Some(LINUX), &selection);

        assert!(check.update_available);
        assert!(check.notice.is_none());
        assert!(check.uninstallable_versions.is_empty());
    }

    /// Both release-asset methods pin the selected installable release.
    #[test]
    fn only_the_release_pinning_method_consults_the_release_catalog() {
        assert!(selects_an_installable_release(InstallMethod::Binary));
        assert!(selects_an_installable_release(InstallMethod::Secondary));
        assert!(!selects_an_installable_release(InstallMethod::Homebrew));
        assert!(!selects_an_installable_release(InstallMethod::Source));
        assert!(!selects_an_installable_release(InstallMethod::Unknown));
    }

    fn check(current: &str, latest: Option<&str>, update_available: bool) -> VersionCheck {
        VersionCheck {
            command: "upgrade.check".to_string(),
            current_version: current.to_string(),
            latest_version: latest.map(str::to_string),
            update_available,
            install_method: InstallMethod::Binary,
            target: None,
            newest_version: None,
            uninstallable_versions: Vec::new(),
            notice: None,
            runners: Vec::new(),
        }
    }

    /// #15733: `--runner-only --check` converges runners to the controller in
    /// place; a plain check targets the release the controller would install.
    #[test]
    fn runner_check_targets_the_version_runners_converge_to() {
        assert_eq!(
            runner_check_target_version(&check("0.417.15", Some("0.418.0"), true), true),
            "0.417.15"
        );
        assert_eq!(
            runner_check_target_version(&check("0.417.15", Some("0.418.0"), true), false),
            "0.418.0"
        );
        assert_eq!(
            runner_check_target_version(&check("0.417.15", Some("0.417.15"), false), false),
            "0.417.15"
        );
    }

    #[test]
    fn only_in_scope_unconverged_service_units_block_convergence() {
        let unit = |scope: &str, status: &str| crate::upgrade::RunnerServiceBinaryEntry {
            unit: "u.service".to_string(),
            scope: scope.to_string(),
            binary: "/b".to_string(),
            binary_version: None,
            running_version: None,
            active_state: None,
            target_version: "0.417.15".to_string(),
            status: status.to_string(),
            reason: None,
            recovery_commands: Vec::new(),
        };
        assert!(unit("controller", "stale").blocks_convergence());
        assert!(unit("legacy", "stale").blocks_convergence());
        assert!(unit("controller", "pending").blocks_convergence());
        assert!(unit("legacy", "unknown").blocks_convergence());
        assert!(!unit("other_controller", "stale").blocks_convergence());
        assert!(!unit("controller", "current").blocks_convergence());
        assert!(!unit("controller", "refreshed").blocks_convergence());
        assert!(!unit("legacy", "inactive").blocks_convergence());
    }
}
