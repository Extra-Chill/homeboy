//! shorten_path — extracted from report.rs.

use std::path::{Path, PathBuf};
use crate::component::{self, Component};
use crate::deploy;
use crate::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use std::collections::{HashMap, HashSet};
use serde::Serialize;
use crate::project::{self, Project};
use crate::server::{self, Server};
use crate::{changelog, git, is_zero, is_zero_u32, version, Result};
use super::super::{build_component_info, path_is_parent_of, ComponentGap, ContextOutput};
use super::ComponentWithState;
use super::ComponentSummary;


pub(crate) fn shorten_path(path: &str, cwd: Option<&PathBuf>) -> String {
    let path_buf = PathBuf::from(path);
    if let Some(cwd_path) = cwd {
        if let Ok(relative) = path_buf.strip_prefix(cwd_path) {
            let rel_str = relative.to_string_lossy().to_string();
            if !rel_str.is_empty() {
                return rel_str;
            }
            return ".".to_string();
        }
    }

    if let Ok(home_str) = std::env::var("HOME") {
        let home = PathBuf::from(&home_str);
        if let Ok(relative) = path_buf.strip_prefix(&home) {
            return format!("~/{}", relative.to_string_lossy());
        }
    }
    path.to_string()
}

pub(crate) fn build_component_summaries(
    components: &[ComponentWithState],
    cwd: Option<&PathBuf>,
) -> Vec<ComponentSummary> {
    components
        .iter()
        .map(|comp| {
            let status = deploy::classify_release_state(comp.release_state.as_ref())
                .as_str()
                .to_string();
            let (commits, code, docs) = comp
                .release_state
                .as_ref()
                .map(|s| (s.commits_since_version, s.code_commits, s.docs_only_commits))
                .unwrap_or((0, 0, 0));

            let mut extensions = comp
                .component
                .extensions
                .as_ref()
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            extensions.sort();

            ComponentSummary {
                id: comp.component.id.clone(),
                path: shorten_path(&comp.component.local_path, cwd),
                extensions,
                status,
                commits_since_version: commits,
                code_commits: code,
                docs_only_commits: docs,
            }
        })
        .collect()
}
