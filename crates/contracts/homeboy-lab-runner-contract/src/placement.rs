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

    #[test]
    fn placement_defaults_to_auto_routing() {
        assert_eq!(Placement::default(), Placement::Auto);
        assert!(!Placement::default().requests_lab());
        assert!(!Placement::default().allows_local_fallback());
    }

    /// Only `lab-or-local` both requests a Lab attempt and permits falling back
    /// to the controller; `lab` requests without permitting, and `local`/`auto`
    /// do neither.
    #[test]
    fn only_lab_or_local_both_requests_lab_and_permits_local_fallback() {
        let matrix = [
            (Placement::Auto, false, false),
            (Placement::Local, false, false),
            (Placement::Lab, true, false),
            (Placement::LabOrLocal, true, true),
        ];

        for (placement, requests_lab, allows_fallback) in matrix {
            assert_eq!(
                placement.requests_lab(),
                requests_lab,
                "{placement:?} requests_lab"
            );
            assert_eq!(
                placement.allows_local_fallback(),
                allows_fallback,
                "{placement:?} allows_local_fallback"
            );
        }
    }
}
