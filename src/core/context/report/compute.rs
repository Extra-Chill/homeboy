//! compute — extracted from report.rs.

use std::collections::{HashMap, HashSet};
use crate::component::{self, Component};
use crate::deploy;
use crate::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::project::{self, Project};
use crate::server::{self, Server};
use crate::{changelog, git, is_zero, is_zero_u32, version, Result};
use super::super::{build_component_info, path_is_parent_of, ComponentGap, ContextOutput};
use super::ContextReportSummary;
use super::ContextReportStatus;
use super::ComponentWithState;
use super::GapSummary;


pub(crate) fn compute_status(
    components: &[ComponentWithState],
    release_buckets: &crate::deploy::ReleaseStateBuckets,
) -> ContextReportStatus {
    let mut config_gaps = 0;
    let mut gap_details = Vec::new();

    for comp in components {
        let id = &comp.component.id;

        for gap in &comp.gaps {
            config_gaps += 1;
            gap_details.push(GapSummary {
                component_id: id.clone(),
                field: gap.field.clone(),
                reason: gap.reason.clone(),
                command: gap.command.clone(),
            });
        }
    }

    ContextReportStatus {
        ready_to_deploy: release_buckets.ready_to_deploy.clone(),
        needs_version_bump: release_buckets.needs_bump.clone(),
        docs_only: release_buckets.docs_only.clone(),
        has_uncommitted: release_buckets.has_uncommitted.clone(),
        config_gaps,
        gap_details,
    }
}

pub(crate) fn compute_summary(components: &[ComponentWithState]) -> ContextReportSummary {
    let mut by_extension: HashMap<String, usize> = HashMap::new();
    let mut by_status: HashMap<String, usize> = HashMap::new();

    for comp in components {
        if let Some(ref extensions) = comp.component.extensions {
            for extension_id in extensions.keys() {
                *by_extension.entry(extension_id.clone()).or_insert(0) += 1;
            }
        }

        let status = deploy::classify_release_state(comp.release_state.as_ref())
            .as_str()
            .to_string();
        *by_status.entry(status).or_insert(0) += 1;
    }

    ContextReportSummary {
        total_components: components.len(),
        by_extension,
        by_status,
    }
}
