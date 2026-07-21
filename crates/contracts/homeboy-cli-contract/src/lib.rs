//! Shared CLI contract types.
//!
//! These types live at the boundary between the CLI argument surface and core
//! routing logic. They are extracted into their own crate so `core` can depend
//! on them without depending on the full `commands`/clap CLI definition (which
//! would create a `core -> commands` dependency edge).

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// The requested execution location. This is normalized once at the CLI
/// boundary and is the only placement input used by routing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[value(rename_all = "lower")]
pub enum Placement {
    Auto,
    Local,
    Lab,
    #[value(name = "lab-or-local")]
    LabOrLocal,
}

impl Default for Placement {
    fn default() -> Self {
        Self::Auto
    }
}

impl Placement {
    /// Explicitly permit controller execution when an intended Lab offload
    /// cannot proceed. `Auto` retains the existing default routing behavior.
    pub const fn allows_local_fallback(self) -> bool {
        matches!(self, Self::LabOrLocal)
    }

    /// Whether the operator requested a Lab attempt instead of leaving the
    /// command to its automatic routing policy.
    pub const fn requests_lab(self) -> bool {
        matches!(self, Self::Lab | Self::LabOrLocal)
    }
}
