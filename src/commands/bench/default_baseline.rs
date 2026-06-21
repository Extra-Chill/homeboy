//! Default-baseline expansion for single-rig bench runs.
//!
//! A rig spec may declare `bench.default_baseline_rig`. When a user runs a
//! single `--rig <candidate>` (and has not opted out), the run is rewritten
//! into the canonical `[baseline, candidate]` comparison shape. This module
//! owns that detection/rewrite logic plus the failure-context helpers that
//! annotate errors and comparison output when the implicit baseline rig
//! fails.

use homeboy::core::extension::bench::{BenchComparisonOutput, BenchDefaultBaselineExpansion};
use homeboy::core::rig;

use super::{matrix, BenchRunArgs};

pub(super) fn add_default_baseline_failure_hint(
    error: homeboy::core::Error,
    metadata: Option<&BenchDefaultBaselineExpansion>,
) -> homeboy::core::Error {
    let Some(metadata) = metadata else {
        return error;
    };
    if error.details.get("rig_id").and_then(|value| value.as_str())
        != Some(metadata.baseline_rig.as_str())
    {
        return error;
    }

    error.with_hint(format!(
        "Implicit default baseline rig '{}' failed before requested rig '{}' could complete. The run plan was injected by bench.default_baseline_rig; pass {} to run only '{}'.",
        metadata.baseline_rig,
        metadata.candidate_rig,
        metadata.opt_out_flag,
        metadata.candidate_rig,
    ))
}

pub(super) fn apply_default_baseline_failure_context(
    output: &mut BenchComparisonOutput,
    metadata: &BenchDefaultBaselineExpansion,
) {
    let mut baseline_failed = false;
    for failure in &mut output.failures {
        if failure.rig_id == metadata.baseline_rig {
            failure.implicit_default_baseline = true;
            baseline_failed = true;
        }
    }
    if !baseline_failed {
        return;
    }

    let hints = output.hints.get_or_insert_with(Vec::new);
    hints.insert(
        0,
        format!(
            "Implicit default baseline rig '{}' failed while preparing comparison for requested rig '{}'. The baseline was injected by bench.default_baseline_rig; pass {} to run only '{}'.",
            metadata.baseline_rig,
            metadata.candidate_rig,
            metadata.opt_out_flag,
            metadata.candidate_rig,
        ),
    );
}

/// Resolve the candidate rig's `bench.default_baseline_rig` and, when
/// applicable, return the rewritten `[baseline, candidate]` rig list
/// the comparison path should run. Returns `None` when no expansion
/// applies — the caller falls through to its normal dispatch.
///
/// Expansion applies when ALL of the following hold:
/// - exactly one `--rig` was passed,
/// - that rig's spec declares a non-empty `bench.default_baseline_rig`,
/// - none of `--baseline` / `--ratchet` / `--ignore-default-baseline`
///   are set.
///
/// A spec that names itself as its own default baseline is rejected
/// with `validation_invalid_argument` — the auto-upgrade would loop
/// and the user almost certainly meant a different rig.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct DefaultBaselineExpansion {
    pub(super) baseline_rig: String,
    pub(super) candidate_rig: String,
    pub(super) rig_ids: Vec<String>,
}

impl DefaultBaselineExpansion {
    pub(super) fn metadata(&self, execution_order: Vec<String>) -> BenchDefaultBaselineExpansion {
        BenchDefaultBaselineExpansion {
            baseline_rig: self.baseline_rig.clone(),
            candidate_rig: self.candidate_rig.clone(),
            execution_order,
            opt_out_flag: "--ignore-default-baseline",
        }
    }
}

pub(super) fn default_baseline_notice(metadata: &BenchDefaultBaselineExpansion) -> String {
    format!(
        "Rig {} declares default baseline rig {}.\nRunning rigs in order: {}.\nUse {} to run only {}.",
        metadata.candidate_rig,
        metadata.baseline_rig,
        metadata.execution_order.join(" -> "),
        metadata.opt_out_flag,
        metadata.candidate_rig,
    )
}

pub(super) fn maybe_expand_default_baseline(
    args: &BenchRunArgs,
) -> homeboy::core::Result<Option<DefaultBaselineExpansion>> {
    if args.rig.len() != 1 {
        return Ok(None);
    }
    if args.baseline_args.baseline || args.baseline_args.ratchet || args.ignore_default_baseline {
        return Ok(None);
    }

    let candidate = &args.rig[0];
    let candidate_spec = rig::load(candidate)?;
    if args.comp.id().is_none()
        && candidate_spec
            .bench
            .as_ref()
            .map(|b| matrix::bench_component_ids(b).len() > 1)
            .unwrap_or(false)
    {
        return Ok(None);
    }
    let Some(baseline_rig_id) = candidate_spec
        .bench
        .as_ref()
        .and_then(|b| b.default_baseline_rig.clone())
    else {
        return Ok(None);
    };

    if baseline_rig_id == *candidate {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "bench.default_baseline_rig",
            format!(
                "rig '{}' declares itself as its own default_baseline_rig; \
                 fix the rig spec or pass --ignore-default-baseline",
                candidate
            ),
            None,
            None,
        ));
    }

    Ok(Some(DefaultBaselineExpansion {
        rig_ids: vec![baseline_rig_id.clone(), candidate.clone()],
        baseline_rig: baseline_rig_id,
        candidate_rig: candidate.clone(),
    }))
}
