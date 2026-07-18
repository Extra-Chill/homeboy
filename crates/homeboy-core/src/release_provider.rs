//! Release/deploy provider hook.
//!
//! Core's status-reporting mechanics (fleet / project / context / git change
//! reporting / tag-gap detection) read release + deploy behavior: they compute
//! per-component deploy status, resolve component versions and tag prefixes,
//! calculate/bucket/classify release state, and read changelog snapshots.
//!
//! Computing all of that is release/deploy behavior, so it is inverted behind
//! this provider: core owns the status models and reporting, the release layer
//! supplies the computed data. With no provider registered (no release
//! subsystem present) the no-op reports empty/None so status reporting degrades
//! gracefully rather than failing.

use std::sync::Mutex;

use homeboy_release_contract::{
    ChangelogSnapshotData, ComponentDeployStatus, ComponentVersionSnapshot,
    FinalizedReleaseSnapshot, ReleaseState, ReleaseStateBuckets, ReleaseStateStatus,
};

use crate::component::Component;
use crate::Result;

/// A component paired with its (optional) already-computed release state, for
/// bucketing. Mirrors what `context` passes into `bucket_release_states`.
pub struct ReleaseStateEntry<'a> {
    pub component_id: &'a str,
    pub release_state: Option<&'a ReleaseState>,
}

/// Assembled changelog info for a component: unreleased entry count + path.
#[derive(Debug, Clone)]
pub struct ChangelogInfoData {
    pub unreleased_entries: usize,
    pub path: String,
}

/// Supplies release/deploy behavior to core's status mechanics.
pub trait ReleaseProvider: Send + Sync {
    /// Per-component deploy status for a project (wraps a no-pull deploy check).
    fn deploy_component_statuses(&self, project_id: &str) -> Result<Vec<ComponentDeployStatus>>;

    /// Compute a component's release state (commits since last version tag).
    fn calculate_release_state(&self, component: &Component) -> Option<ReleaseState>;

    /// Classify a release state into a high-level status.
    fn classify_release_state(&self, state: Option<&ReleaseState>) -> ReleaseStateStatus;

    /// Bucket components by release state.
    fn bucket_release_states(&self, entries: &[ReleaseStateEntry<'_>]) -> ReleaseStateBuckets;

    /// A component's configured version.
    fn get_component_version(&self, component: &Component) -> Option<String>;

    /// A component's tag prefix (e.g. `homeboy-v`).
    fn component_tag_prefix(&self, component: &Component) -> Option<String>;

    /// A component's latest release tag, if any.
    fn latest_component_tag(&self, component: &Component) -> Option<String>;

    /// Read a component's version snapshot (id + version + targets).
    fn read_component_version_snapshot(
        &self,
        component: &Component,
    ) -> Option<ComponentVersionSnapshot>;

    /// Version-init warnings for a component (misconfigured version targets etc).
    fn build_version_init_warnings(&self, component: &Component) -> Vec<String>;

    /// Validate that a component's source version aligns with its git baseline.
    fn validate_baseline_alignment(
        &self,
        version: Option<&ComponentVersionSnapshot>,
        baseline_ref: Option<&str>,
    ) -> Option<String>;

    /// Read a component's finalized-release + unreleased changelog snapshots.
    #[allow(clippy::type_complexity)]
    fn read_changelog_snapshots(
        &self,
        component: &Component,
    ) -> Option<(
        Option<FinalizedReleaseSnapshot>,
        Option<ChangelogSnapshotData>,
    )>;

    /// Assemble a component's changelog info (unreleased count + path).
    fn changelog_info(&self, component: &Component) -> Option<ChangelogInfoData>;
}

struct NoopProvider;

impl ReleaseProvider for NoopProvider {
    fn deploy_component_statuses(&self, _project_id: &str) -> Result<Vec<ComponentDeployStatus>> {
        Ok(Vec::new())
    }
    fn calculate_release_state(&self, _component: &Component) -> Option<ReleaseState> {
        None
    }
    fn classify_release_state(&self, _state: Option<&ReleaseState>) -> ReleaseStateStatus {
        ReleaseStateStatus::Unknown
    }
    fn bucket_release_states(&self, _entries: &[ReleaseStateEntry<'_>]) -> ReleaseStateBuckets {
        ReleaseStateBuckets::default()
    }
    fn get_component_version(&self, _component: &Component) -> Option<String> {
        None
    }
    fn component_tag_prefix(&self, _component: &Component) -> Option<String> {
        None
    }
    fn latest_component_tag(&self, _component: &Component) -> Option<String> {
        None
    }
    fn read_component_version_snapshot(
        &self,
        _component: &Component,
    ) -> Option<ComponentVersionSnapshot> {
        None
    }
    fn build_version_init_warnings(&self, _component: &Component) -> Vec<String> {
        Vec::new()
    }
    fn validate_baseline_alignment(
        &self,
        _version: Option<&ComponentVersionSnapshot>,
        _baseline_ref: Option<&str>,
    ) -> Option<String> {
        None
    }
    fn read_changelog_snapshots(
        &self,
        _component: &Component,
    ) -> Option<(
        Option<FinalizedReleaseSnapshot>,
        Option<ChangelogSnapshotData>,
    )> {
        None
    }
    fn changelog_info(&self, _component: &Component) -> Option<ChangelogInfoData> {
        None
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn ReleaseProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn ReleaseProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the release provider. Called once at startup by the release layer.
pub fn register_release_provider(provider: Box<dyn ReleaseProvider>) {
    let mut slot = provider_slot().lock().expect("release provider lock");
    *slot = Some(provider);
}

fn with_provider<T>(f: impl FnOnce(&dyn ReleaseProvider) -> T) -> T {
    let slot = provider_slot().lock().expect("release provider lock");
    match slot.as_deref() {
        Some(provider) => f(provider),
        None => f(&NoopProvider),
    }
}

pub(crate) fn deploy_component_statuses(project_id: &str) -> Result<Vec<ComponentDeployStatus>> {
    with_provider(|p| p.deploy_component_statuses(project_id))
}

pub(crate) fn calculate_release_state(component: &Component) -> Option<ReleaseState> {
    with_provider(|p| p.calculate_release_state(component))
}

pub(crate) fn classify_release_state(state: Option<&ReleaseState>) -> ReleaseStateStatus {
    with_provider(|p| p.classify_release_state(state))
}

pub(crate) fn bucket_release_states(entries: &[ReleaseStateEntry<'_>]) -> ReleaseStateBuckets {
    with_provider(|p| p.bucket_release_states(entries))
}

pub(crate) fn get_component_version(component: &Component) -> Option<String> {
    with_provider(|p| p.get_component_version(component))
}

pub(crate) fn component_tag_prefix(component: &Component) -> Option<String> {
    with_provider(|p| p.component_tag_prefix(component))
}

pub(crate) fn latest_component_tag(component: &Component) -> Option<String> {
    with_provider(|p| p.latest_component_tag(component))
}

pub(crate) fn read_component_version_snapshot(
    component: &Component,
) -> Option<ComponentVersionSnapshot> {
    with_provider(|p| p.read_component_version_snapshot(component))
}

pub(crate) fn build_version_init_warnings(component: &Component) -> Vec<String> {
    with_provider(|p| p.build_version_init_warnings(component))
}

pub(crate) fn validate_baseline_alignment(
    version: Option<&ComponentVersionSnapshot>,
    baseline_ref: Option<&str>,
) -> Option<String> {
    with_provider(|p| p.validate_baseline_alignment(version, baseline_ref))
}

#[allow(clippy::type_complexity)]
pub(crate) fn read_changelog_snapshots(
    component: &Component,
) -> Option<(
    Option<FinalizedReleaseSnapshot>,
    Option<ChangelogSnapshotData>,
)> {
    with_provider(|p| p.read_changelog_snapshots(component))
}

pub(crate) fn changelog_info(component: &Component) -> Option<ChangelogInfoData> {
    with_provider(|p| p.changelog_info(component))
}
