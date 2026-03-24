//! fix_accumulator — extracted from planner.rs.

use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::Error;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use super::super::verify::AuditConvergenceScoring;
use std::time::{SystemTime, UNIX_EPOCH};


#[derive(Default)]
pub(crate) struct FixAccumulator {
    fixes: Vec<FixApplied>,
}
