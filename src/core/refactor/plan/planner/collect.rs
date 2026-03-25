//! collect — extracted from planner.rs.

use std::collections::{BTreeSet, HashSet};
use crate::code_audit::CodeAuditResult;
use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::Error;
use serde::Serialize;
use std::path::{Path, PathBuf};
use super::super::verify::AuditConvergenceScoring;
use super::FixProposal;
use super::PlannedStage;
use super::PlanStageSummary;


pub(crate) fn collect_fix_proposals(stages: &[PlannedStage]) -> Vec<FixProposal> {
    let mut proposals = Vec::new();

    for stage in stages {
        for fix in &stage.fix_results {
            proposals.push(FixProposal {
                source: stage.source.clone(),
                file: fix.file.clone(),
                rule_id: fix.rule.clone(),
                action: fix.action.clone(),
            });
        }
    }

    proposals.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.file.cmp(&b.file))
            .then(a.rule_id.cmp(&b.rule_id))
    });

    proposals
}

pub(crate) fn collect_stage_changed_files(stages: &[PlanStageSummary]) -> Vec<String> {
    let mut final_changed_files = BTreeSet::new();
    for stage in stages {
        for file in &stage.changed_files {
            final_changed_files.insert(file.clone());
        }
    }
    final_changed_files.into_iter().collect()
}
