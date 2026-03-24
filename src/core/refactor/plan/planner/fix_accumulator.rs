//! fix_accumulator — extracted from planner.rs.

use crate::refactor::auto::{self, FixApplied, FixResultsSummary};


#[derive(Default)]
pub(crate) struct FixAccumulator {
    fixes: Vec<FixApplied>,
}
