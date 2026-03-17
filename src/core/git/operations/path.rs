//! path — extracted from operations.rs.

use crate::component;
use crate::error::{Error, Result};
use super::detect_baseline_with_version;
use super::BaselineInfo;
use super::super::changes::*;
use super::super::commits::*;
use super::super::*;


/// Detect baseline for a path (public wrapper).
/// For version-aware baseline detection, use detect_baseline_with_version().
pub(crate) fn detect_baseline_for_path(path: &str) -> Result<BaselineInfo> {
    detect_baseline_with_version(path, None)
}

pub(crate) fn get_component_path(component_id: &str) -> Result<String> {
    let comp = component::resolve_effective(Some(component_id), None, None)?;
    Ok(comp.local_path)
}
