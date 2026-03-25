//! validate_version — extracted from report.rs.

use crate::component::{self, Component};
use crate::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use crate::{changelog, git, is_zero, is_zero_u32, version, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::project::{self, Project};
use crate::server::{self, Server};
use super::super::{build_component_info, path_is_parent_of, ComponentGap, ContextOutput};


pub(crate) fn validate_version_targets(components: &[ComponentWithState]) -> Vec<String> {
    components
        .iter()
        .flat_map(|wrapper| version::build_init_warnings(&wrapper.component))
        .collect()
}

pub(crate) fn validate_version_baseline_alignment(
    version: &Option<VersionSnapshot>,
    git: &Option<GitSnapshot>,
) -> Option<String> {
    let version_snapshot = version
        .as_ref()
        .map(|snapshot| version::ComponentVersionSnapshot {
            component_id: snapshot.component_id.clone(),
            version: snapshot.version.clone(),
            targets: snapshot.targets.clone(),
        });

    version::validate_baseline_alignment(
        version_snapshot.as_ref(),
        git.as_ref()
            .and_then(|snapshot| snapshot.baseline_ref.as_deref()),
    )
}
