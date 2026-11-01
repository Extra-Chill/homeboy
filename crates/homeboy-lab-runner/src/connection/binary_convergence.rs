//! Runner-binary convergence: bring a runner's configured Homeboy binary to a
//! target controller release. `homeboy runner connect` converges to its own
//! build before attaching (#16825); `homeboy upgrade --upgrade-runner`
//! converges to the controller it just installed, because the process running
//! the upgrade is still the old controller (#16845).
//!
//! Convergence is one decision over three observations: the target
//! controller build, the configured remote binary's identity, and the
//! daemon's active jobs. Installation reuses the verified release path of
//! `refresh-homeboy` pinned to the target's exact version tag, so it never
//! resolves "latest" and never builds from source. That refresh owns its own
//! runtime-promotion lease and replaces the daemon through its existing
//! `--reconnect` mechanism (in-place restart or service-unit repoint). It runs
//! before connect takes its own lease, and refresh's reconnect attaches
//! through the non-converging connect, so convergence never re-enters itself.

use homeboy_product_identity::BuildIdentity;

use super::remote_daemon::{remote_homeboy_identity, resolve_ssh_runner};
use super::{
    active_jobs_from_replacement_observation, load, normalize_homeboy_version_owned,
    reconcile_status, remote_runner_homeboy_path,
};
use crate::homeboy_refresh::{install_controller_release, ControllerReleaseInstall};
use crate::{RunnerBinaryConvergence, RunnerBinaryConvergenceStatus};

/// The configured remote binary, as its identity probe reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedRemoteBinary {
    pub(crate) version: String,
    pub(crate) build_identity: Option<String>,
}

/// Converge `runner_id`'s configured binary on the `controller` release.
/// Never fails: every outcome, including an install failure, is reported so
/// the caller decides what follows from whichever binary is configured
/// afterwards.
pub(crate) fn converge_runner_binary(
    runner_id: &str,
    controller: &BuildIdentity,
) -> RunnerBinaryConvergence {
    converge_runner_binary_with(
        controller,
        || configured_remote_binary(runner_id),
        || active_daemon_job_ids(runner_id),
        || install_controller_release(runner_id, controller),
    )
}

/// [`converge_runner_binary`] with its observations and install injected.
pub(crate) fn converge_runner_binary_with(
    controller: &BuildIdentity,
    remote: impl FnOnce() -> Result<ObservedRemoteBinary, String>,
    active_jobs: impl FnOnce() -> Result<Vec<String>, String>,
    install: impl FnOnce() -> ControllerReleaseInstall,
) -> RunnerBinaryConvergence {
    let to = Some(controller.display.clone());
    let report = |status, from: Option<String>, detail: Option<String>| RunnerBinaryConvergence {
        status,
        from,
        to: to.clone(),
        detail,
        blocking_job_ids: Vec::new(),
    };
    // A dev or source build has no published release to install; the runner
    // binary is left alone and not even probed.
    let Some(controller_version) = controller_release_version(controller) else {
        return report(
            RunnerBinaryConvergenceStatus::SkippedDevBuild,
            None,
            Some("the controller is not a published release build".to_string()),
        );
    };
    let remote = match remote() {
        Ok(remote) => remote,
        Err(detail) => {
            return report(
                RunnerBinaryConvergenceStatus::SkippedUnverifiable,
                None,
                Some(detail),
            )
        }
    };
    let from = Some(
        remote
            .build_identity
            .clone()
            .unwrap_or_else(|| format!("homeboy {}", remote.version)),
    );
    let Ok(remote_version) =
        semver::Version::parse(&normalize_homeboy_version_owned(&remote.version))
    else {
        return report(
            RunnerBinaryConvergenceStatus::SkippedUnverifiable,
            from,
            Some(format!(
                "the runner's homeboy reported an unparseable version `{}`",
                remote.version
            )),
        );
    };
    // Same release version: the runner is current. A different build commit
    // at the same version is an explicit dev-sync or select, which
    // convergence must not override.
    if remote_version == controller_version {
        return report(RunnerBinaryConvergenceStatus::AlreadyCurrent, from, None);
    }
    if remote_version > controller_version {
        return report(
            RunnerBinaryConvergenceStatus::SkippedRemoteNewer,
            from,
            Some("the runner's homeboy is newer than the controller; never downgraded".to_string()),
        );
    }
    match active_jobs() {
        Err(detail) => {
            return report(
                RunnerBinaryConvergenceStatus::Failed,
                from,
                Some(format!("could not list the daemon's active jobs: {detail}")),
            )
        }
        Ok(job_ids) if !job_ids.is_empty() => return pending(from, to, job_ids),
        Ok(_) => {}
    }
    match install() {
        ControllerReleaseInstall::Installed { identity } => RunnerBinaryConvergence {
            status: RunnerBinaryConvergenceStatus::Converged,
            from,
            to: identity.or(to),
            detail: None,
            blocking_job_ids: Vec::new(),
        },
        ControllerReleaseInstall::DeferredByActiveJobs(job_ids) => pending(from, to, job_ids),
        ControllerReleaseInstall::Failed(detail) => {
            report(RunnerBinaryConvergenceStatus::Failed, from, Some(detail))
        }
    }
}

fn pending(
    from: Option<String>,
    to: Option<String>,
    job_ids: Vec<String>,
) -> RunnerBinaryConvergence {
    RunnerBinaryConvergence {
        status: RunnerBinaryConvergenceStatus::PendingActiveJobs,
        from,
        to,
        detail: Some(format!(
            "the daemon has active jobs ({}); the runner binary was left in place and converges once they finish",
            job_ids.join(", ")
        )),
        blocking_job_ids: job_ids,
    }
}

/// The controller's release version when it is a published release build: a
/// plain `MAJOR.MINOR.PATCH` version built from a clean, known commit.
fn controller_release_version(controller: &BuildIdentity) -> Option<semver::Version> {
    if controller.git_commit.is_none() || controller.git_dirty != Some(false) {
        return None;
    }
    semver::Version::parse(&controller.version)
        .ok()
        .filter(|version| version.pre.is_empty() && version.build.is_empty())
}

fn configured_remote_binary(runner_id: &str) -> Result<ObservedRemoteBinary, String> {
    let runner = load(runner_id).map_err(|error| error.message)?;
    let homeboy = remote_runner_homeboy_path(&runner, "runner binary convergence")
        .map_err(|error| error.message)?;
    let Some((_, _, client)) = resolve_ssh_runner(&runner).map_err(|error| error.message)? else {
        return Err("binary convergence applies to SSH runners only".to_string());
    };
    let identity = remote_homeboy_identity(&client, homeboy)?;
    Ok(ObservedRemoteBinary {
        version: identity.version,
        build_identity: identity.build_identity,
    })
}

fn active_daemon_job_ids(runner_id: &str) -> Result<Vec<String>, String> {
    let status = reconcile_status(runner_id).map_err(|error| error.message)?;
    active_jobs_from_replacement_observation(status)
        .map(|jobs| jobs.into_iter().map(|job| job.job_id).collect())
        .map_err(|error| error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(version: &str, commit: Option<&str>, dirty: Option<bool>) -> BuildIdentity {
        BuildIdentity {
            version: version.to_string(),
            git_commit: commit.map(str::to_string),
            git_dirty: dirty,
            display: format!("homeboy {version}+{}", commit.unwrap_or("unknown")),
        }
    }

    fn release() -> BuildIdentity {
        controller("0.439.0", Some("abc1234"), Some(false))
    }

    fn remote(version: &str) -> Result<ObservedRemoteBinary, String> {
        Ok(ObservedRemoteBinary {
            version: version.to_string(),
            build_identity: Some(format!("homeboy {version}+def5678")),
        })
    }

    fn no_jobs() -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    fn never_installs() -> ControllerReleaseInstall {
        panic!("convergence must not install")
    }

    #[test]
    fn converges_when_the_remote_is_older() {
        let report = converge_runner_binary_with(
            &release(),
            || remote("0.438.2"),
            no_jobs,
            || ControllerReleaseInstall::Installed {
                identity: Some("homeboy 0.439.0+abc1234".to_string()),
            },
        );
        assert_eq!(report.status, RunnerBinaryConvergenceStatus::Converged);
        assert_eq!(report.from.as_deref(), Some("homeboy 0.438.2+def5678"));
        assert_eq!(report.to.as_deref(), Some("homeboy 0.439.0+abc1234"));
        assert!(report.blocking_job_ids.is_empty());
    }

    #[test]
    fn skips_when_the_remote_runs_the_controller_release() {
        let report =
            converge_runner_binary_with(&release(), || remote("0.439.0"), no_jobs, never_installs);
        assert_eq!(report.status, RunnerBinaryConvergenceStatus::AlreadyCurrent);
    }

    #[test]
    fn never_downgrades_a_newer_remote() {
        let report =
            converge_runner_binary_with(&release(), || remote("0.440.0"), no_jobs, never_installs);
        assert_eq!(
            report.status,
            RunnerBinaryConvergenceStatus::SkippedRemoteNewer
        );
    }

    #[test]
    fn a_dev_build_controller_keeps_todays_behavior_without_probing() {
        for dev in [
            controller("0.439.0", Some("abc1234"), Some(true)),
            controller("0.439.0", None, None),
            controller("0.440.0-dev", Some("abc1234"), Some(false)),
        ] {
            let report = converge_runner_binary_with(
                &dev,
                || panic!("a dev build must not probe the runner"),
                || panic!("a dev build must not list jobs"),
                never_installs,
            );
            assert_eq!(
                report.status,
                RunnerBinaryConvergenceStatus::SkippedDevBuild
            );
            assert_eq!(report.from, None);
        }
    }

    #[test]
    fn active_jobs_hold_convergence_pending_and_name_the_jobs() {
        let report = converge_runner_binary_with(
            &release(),
            || remote("0.438.2"),
            || Ok(vec!["job-1".to_string(), "job-2".to_string()]),
            never_installs,
        );
        assert_eq!(
            report.status,
            RunnerBinaryConvergenceStatus::PendingActiveJobs
        );
        assert_eq!(report.blocking_job_ids, ["job-1", "job-2"]);
        assert!(report.detail.unwrap().contains("job-1, job-2"));
    }

    #[test]
    fn a_refresh_deferred_by_jobs_that_appeared_is_pending() {
        let report = converge_runner_binary_with(
            &release(),
            || remote("0.438.2"),
            no_jobs,
            || ControllerReleaseInstall::DeferredByActiveJobs(vec!["job-3".to_string()]),
        );
        assert_eq!(
            report.status,
            RunnerBinaryConvergenceStatus::PendingActiveJobs
        );
        assert_eq!(report.blocking_job_ids, ["job-3"]);
    }

    #[test]
    fn an_install_failure_is_reported_not_raised() {
        let report = converge_runner_binary_with(
            &release(),
            || remote("0.438.2"),
            no_jobs,
            || ControllerReleaseInstall::Failed("checksum mismatch".to_string()),
        );
        assert_eq!(report.status, RunnerBinaryConvergenceStatus::Failed);
        assert_eq!(report.detail.as_deref(), Some("checksum mismatch"));
        assert_eq!(report.from.as_deref(), Some("homeboy 0.438.2+def5678"));
        assert_eq!(report.to.as_deref(), Some("homeboy 0.439.0+abc1234"));
    }

    #[test]
    fn an_unreadable_remote_identity_is_unverifiable() {
        let report = converge_runner_binary_with(
            &release(),
            || Err("ssh exited 255".to_string()),
            no_jobs,
            never_installs,
        );
        assert_eq!(
            report.status,
            RunnerBinaryConvergenceStatus::SkippedUnverifiable
        );
    }

    #[test]
    fn refresh_reconnect_never_reenters_convergence() {
        let refresh = include_str!("../homeboy_refresh.rs");
        assert!(!refresh.contains("connect_converging_binary"));
        assert!(!refresh.contains("converge_runner_binary"));
    }
}
