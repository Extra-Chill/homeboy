//! release_state_status — extracted from types.rs.

use serde::Serialize;
use crate::component::Component;
use crate::error::Result;


/// High-level status derived from a component release state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStateStatus {
    Uncommitted,
    NeedsBump,
    DocsOnly,
    Clean,
    Unknown,
}
