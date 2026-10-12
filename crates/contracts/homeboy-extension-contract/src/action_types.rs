//! Action-type contract enums for extension manifests.
//!
//! Pure serde enums describing the shape of manifest-declared actions. The CLI
//! parses these; execution behavior lives in `homeboy_core::extension`.

use serde::{Deserialize, Serialize};

/// Type of action that can be executed by a extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActionType {
    Command,
    Builtin,
}

/// Builtin action types for Desktop app (copy, export operations).
/// CLI parses these but does not execute them - Desktop implements the behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinAction {
    CopyColumn,
    ExportCsv,
    CopyJson,
}
