use serde::{Deserialize, Serialize};

/// Durable diagnostic evidence for a declared Cargo target selection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CargoTargetEvidence {
    pub path: String,
    pub resolution: String,
    pub owner: String,
}
