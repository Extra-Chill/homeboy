//! In-core implementation of the `ReleaseProvider` hook.
//!
//! Deploy + release still live inside `homeboy-core` during the extraction prep
//! phase, so this impl delegates straight to the in-core deploy/release fns and
//! registers itself at startup. When deploy/release move out into the
//! `homeboy-release` crate, this impl moves with them and registers from the
//! CLI runtime instead — the hook seam (and all of core's status mechanics)
//! stays unchanged.

use homeboy_release_contract::{
    ChangelogSnapshotData, ComponentDeployStatus, ComponentVersionSnapshot,
    FinalizedReleaseSnapshot, ReleaseState, ReleaseStateBuckets, ReleaseStateStatus,
};

use crate::deploy::{self, DeployConfig};
use crate::release::{changelog, version};
use homeboy_core::component::Component;
use homeboy_core::release_provider::{
    register_release_provider, ChangelogInfoData, ReleaseProvider, ReleaseStateEntry,
};
use homeboy_core::Result;

struct CoreReleaseProvider;

impl ReleaseProvider for CoreReleaseProvider {
    fn deploy_component_statuses(&self, project_id: &str) -> Result<Vec<ComponentDeployStatus>> {
        let config = DeployConfig::check_all_no_pull_head();
        let result = deploy::run(project_id, &config)?;
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
        deploy::calculate_release_state(component)
    }

    fn classify_release_state(&self, state: Option<&ReleaseState>) -> ReleaseStateStatus {
        deploy::classify_release_state(state)
    }

    fn bucket_release_states(&self, entries: &[ReleaseStateEntry<'_>]) -> ReleaseStateBuckets {
        deploy::bucket_release_states(entries.iter().map(|e| (e.component_id, e.release_state)))
    }

    fn get_component_version(&self, component: &Component) -> Option<String> {
        version::get_component_version(component)
    }

    fn component_tag_prefix(&self, component: &Component) -> Option<String> {
        crate::release::component_tag_prefix(component)
            .ok()
            .flatten()
    }

    fn latest_component_tag(&self, component: &Component) -> Option<String> {
        crate::release::latest_component_tag(component)
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
        crate::release::version::validate_baseline_alignment(version, baseline_ref)
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
