//! Implementation of core's `ReleaseProvider` hook.
//!
//! Despite the name, every method this hook needs is answerable at or below the
//! deploy layer: the deploy-shaped methods come from this crate, and the
//! version/tag/changelog methods come from `homeboy-version`. Nothing here
//! needs `homeboy-release`, which is why it lives on this side of the split —
//! it lets `homeboy-release` depend on `homeboy-deploy` one-way, and lets this
//! crate's own tests register a real provider without a circular dependency
//! (#10126).
//!
//! `homeboy-release` re-exports this module, so the CLI registration path and
//! all of core's status mechanics are unchanged.

use homeboy_release_contract::{
    ChangelogSnapshotData, ComponentDeployStatus, ComponentVersionSnapshot,
    FinalizedReleaseSnapshot, ReleaseState, ReleaseStateBuckets, ReleaseStateStatus,
};

use crate::DeployConfig;
use homeboy_core::component::Component;
use homeboy_core::release_provider::{
    register_release_provider, ChangelogInfoData, ReleaseProvider, ReleaseStateEntry,
};
use homeboy_core::Result;
use homeboy_version::{changelog, version};

struct CoreReleaseProvider;

impl ReleaseProvider for CoreReleaseProvider {
    fn deploy_component_statuses(&self, project_id: &str) -> Result<Vec<ComponentDeployStatus>> {
        let config = DeployConfig::check_all_no_pull_head();
        let result = crate::run(project_id, &config)?;
        Ok(result
            .results
            .into_iter()
            .map(|r| ComponentDeployStatus {
                id: r.id,
                component_status: r.component_status,
                local_version: r.local_version,
                remote_version: r.remote_version,
            })
            .collect())
    }

    fn calculate_release_state(&self, component: &Component) -> Option<ReleaseState> {
        crate::calculate_release_state(component)
    }

    fn classify_release_state(&self, state: Option<&ReleaseState>) -> ReleaseStateStatus {
        crate::classify_release_state(state)
    }

    fn bucket_release_states(&self, entries: &[ReleaseStateEntry<'_>]) -> ReleaseStateBuckets {
        crate::bucket_release_states(entries.iter().map(|e| (e.component_id, e.release_state)))
    }

    fn get_component_version(&self, component: &Component) -> Option<String> {
        version::get_component_version(component)
    }

    fn component_tag_prefix(&self, component: &Component) -> Option<String> {
        homeboy_version::component_tag_prefix(component)
            .ok()
            .flatten()
    }

    fn latest_component_tag(&self, component: &Component) -> Option<String> {
        homeboy_version::latest_component_tag(component)
            .ok()
            .flatten()
    }

    fn read_component_version_snapshot(
        &self,
        component: &Component,
    ) -> Option<ComponentVersionSnapshot> {
        version::read_component_snapshot(component).ok()
    }

    fn build_version_init_warnings(&self, component: &Component) -> Vec<String> {
        version::build_init_warnings(component)
    }

    fn validate_baseline_alignment(
        &self,
        version: Option<&ComponentVersionSnapshot>,
        baseline_ref: Option<&str>,
    ) -> Option<String> {
        homeboy_version::version::validate_baseline_alignment(version, baseline_ref)
    }

    fn read_changelog_snapshots(
        &self,
        component: &Component,
    ) -> Option<(
        Option<FinalizedReleaseSnapshot>,
        Option<ChangelogSnapshotData>,
    )> {
        changelog::read_component_snapshots(component).ok()
    }

    fn changelog_info(&self, component: &Component) -> Option<ChangelogInfoData> {
        let changelog_path = changelog::resolve_changelog_path(component).ok()?;
        let content = std::fs::read_to_string(&changelog_path).ok()?;
        let settings = changelog::resolve_effective_settings(Some(component));
        let unreleased_entries =
            changelog::count_unreleased_entries(&content, &settings.next_section_aliases);
        Some(ChangelogInfoData {
            unreleased_entries,
            path: changelog_path.to_string_lossy().to_string(),
        })
    }
}

/// Register the in-core release provider. Called once at core startup.
pub fn register() {
    register_release_provider(Box::new(CoreReleaseProvider));
}
