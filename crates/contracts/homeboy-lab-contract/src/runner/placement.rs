//! Requested execution placement.
//!
//! [`Placement`] sits at the boundary between the CLI argument surface and core
//! Lab routing. It lives below core, in this crate rather than in the CLI, so
//! `core` can name it without depending on the full `commands`/clap CLI
//! definition (which would create a `core -> commands` dependency edge).
//!
//! It belongs with the rest of the runner contract because every value it takes
//! is a statement about the Lab runner: whether to attempt an offload, and
//! whether controller execution is an acceptable fallback.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// The requested execution location. This is normalized once at the CLI
/// boundary and is the only placement input used by routing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize, Default)]
#[value(rename_all = "lower")]
pub enum Placement {
    #[default]
    Auto,
    Local,
    Lab,
    #[value(name = "lab-or-local")]
    LabOrLocal,
}

impl Placement {
    /// Explicit operator authorization to execute on the controller despite
    /// resource admission. This is distinct from permitting a local fallback
    /// after a Lab attempt.
    pub const fn is_explicit_local_override(self) -> bool {
        matches!(self, Self::Local)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `--placement <value>` is an operator-facing surface: the accepted
    /// spellings are part of the CLI contract, not an implementation detail of
    /// whichever crate happens to hold the enum. Asserted here so relocating
    /// the type cannot quietly rename a flag value.
    #[test]
    fn placement_keeps_its_operator_facing_value_names() {
        let names: Vec<String> = Placement::value_variants()
            .iter()
            .map(|variant| {
                variant
                    .to_possible_value()
                    .expect("every placement variant is selectable")
                    .get_name()
                    .to_string()
            })
            .collect();

        assert_eq!(names, ["auto", "local", "lab", "lab-or-local"]);
    }
}
