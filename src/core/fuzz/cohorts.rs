use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::FuzzHotspotSet;

const CONVERGENCE_TOP_WINDOW: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct FuzzHotspotCohortItem {
    pub key: String,
    pub label: Option<String>,
    pub score: f64,
    pub occurrences: u64,
    pub run_count: usize,
    pub rank: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FuzzHotspotCohortComparison {
    pub baseline_id: String,
    pub candidate_id: String,
    pub item_count: usize,
    pub baseline_drift: FuzzHotspotCohortBaselineDrift,
    pub new_items: usize,
    pub resolved_items: usize,
    pub increased_items: usize,
    pub decreased_items: usize,
    pub unchanged_items: usize,
    pub collapsed_top_items: Vec<String>,
    pub emerging_top_items: Vec<String>,
    pub items: Vec<FuzzHotspotCohortDelta>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FuzzHotspotCohortBaselineDrift {
    pub baseline_score_total: f64,
    pub candidate_score_total: f64,
    pub score_total_delta: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_total_relative_delta: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FuzzHotspotCohortDelta {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub change_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_score_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_lift: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized_score_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank_movement: Option<i64>,
    pub baseline_occurrences: u64,
    pub candidate_occurrences: u64,
    pub occurrence_delta: i64,
    pub baseline_run_count: usize,
    pub candidate_run_count: usize,
    pub run_count_delta: i64,
}

pub fn compare_fuzz_hotspot_sets(
    baseline: &FuzzHotspotSet,
    candidate: &FuzzHotspotSet,
) -> FuzzHotspotCohortComparison {
    compare_fuzz_hotspot_cohorts(
        baseline.id.clone(),
        candidate.id.clone(),
        &cohort_items_from_hotspot_set(baseline),
        &cohort_items_from_hotspot_set(candidate),
    )
}

pub fn compare_fuzz_hotspot_cohorts(
    baseline_id: impl Into<String>,
    candidate_id: impl Into<String>,
    baseline: &[FuzzHotspotCohortItem],
    candidate: &[FuzzHotspotCohortItem],
) -> FuzzHotspotCohortComparison {
    let baseline_by_key = baseline
        .iter()
        .map(|item| (item.key.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let candidate_by_key = candidate
        .iter()
        .map(|item| (item.key.clone(), item))
        .collect::<BTreeMap<_, _>>();

    let mut items = baseline_by_key
        .keys()
        .chain(candidate_by_key.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|key| {
            let before = baseline_by_key.get(&key).copied();
            let after = candidate_by_key.get(&key).copied();
            cohort_delta(key, before, after)
        })
        .collect::<Vec<_>>();

    items.sort_by(|a, b| {
        b.normalized_score_delta
            .unwrap_or(0.0)
            .abs()
            .total_cmp(&a.normalized_score_delta.unwrap_or(0.0).abs())
            .then_with(|| {
                b.score_delta
                    .unwrap_or(0.0)
                    .abs()
                    .total_cmp(&a.score_delta.unwrap_or(0.0).abs())
            })
            .then_with(|| {
                a.candidate_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&b.candidate_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                a.baseline_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&b.baseline_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| a.key.cmp(&b.key))
    });

    let baseline_score_total = baseline.iter().map(|item| item.score).sum::<f64>();
    let candidate_score_total = candidate.iter().map(|item| item.score).sum::<f64>();
    let score_total_delta = candidate_score_total - baseline_score_total;
    let score_total_relative_delta =
        (baseline_score_total != 0.0).then_some(score_total_delta / baseline_score_total.abs());
    let collapsed_top_items = items
        .iter()
        .filter(|item| {
            item.baseline_rank
                .is_some_and(|rank| rank <= CONVERGENCE_TOP_WINDOW)
        })
        .filter(|item| item.change_kind == "resolved" || item.change_kind == "decreased")
        .map(|item| item.key.clone())
        .collect::<Vec<_>>();
    let emerging_top_items = items
        .iter()
        .filter(|item| {
            item.candidate_rank
                .is_some_and(|rank| rank <= CONVERGENCE_TOP_WINDOW)
        })
        .filter(|item| item.change_kind == "new" || item.change_kind == "increased")
        .map(|item| item.key.clone())
        .collect::<Vec<_>>();

    FuzzHotspotCohortComparison {
        baseline_id: baseline_id.into(),
        candidate_id: candidate_id.into(),
        item_count: items.len(),
        baseline_drift: FuzzHotspotCohortBaselineDrift {
            baseline_score_total,
            candidate_score_total,
            score_total_delta,
            score_total_relative_delta,
        },
        new_items: items
            .iter()
            .filter(|item| item.change_kind == "new")
            .count(),
        resolved_items: items
            .iter()
            .filter(|item| item.change_kind == "resolved")
            .count(),
        increased_items: items
            .iter()
            .filter(|item| item.change_kind == "increased")
            .count(),
        decreased_items: items
            .iter()
            .filter(|item| item.change_kind == "decreased")
            .count(),
        unchanged_items: items
            .iter()
            .filter(|item| item.change_kind == "unchanged")
            .count(),
        collapsed_top_items,
        emerging_top_items,
        items,
    }
}

fn cohort_items_from_hotspot_set(set: &FuzzHotspotSet) -> Vec<FuzzHotspotCohortItem> {
    set.items
        .iter()
        .enumerate()
        .map(|(index, item)| FuzzHotspotCohortItem {
            key: item.id.clone(),
            label: item.label.clone(),
            score: item.relative_score.unwrap_or(item.value),
            occurrences: item.sample_count.unwrap_or(1),
            run_count: 1,
            rank: item.rank.unwrap_or(index as u64 + 1) as usize,
        })
        .collect()
}

fn cohort_delta(
    key: String,
    before: Option<&FuzzHotspotCohortItem>,
    after: Option<&FuzzHotspotCohortItem>,
) -> FuzzHotspotCohortDelta {
    let baseline_score = before.map(|item| item.score);
    let candidate_score = after.map(|item| item.score);
    let score_delta = baseline_score
        .zip(candidate_score)
        .map(|(baseline, candidate)| candidate - baseline);
    let relative_lift = baseline_score
        .zip(candidate_score)
        .and_then(|(baseline, candidate)| {
            (baseline != 0.0).then_some((candidate - baseline) / baseline.abs())
        });
    let normalized_score_delta = match (baseline_score, candidate_score) {
        (Some(baseline), Some(candidate)) => {
            let denominator = baseline.abs().max(candidate.abs());
            (denominator != 0.0).then_some((candidate - baseline) / denominator)
        }
        (None, Some(candidate)) => (candidate != 0.0).then_some(candidate.signum()),
        (Some(baseline), None) => (baseline != 0.0).then_some(-baseline.signum()),
        (None, None) => None,
    };
    let rank_movement = before
        .map(|item| item.rank)
        .zip(after.map(|item| item.rank))
        .map(|(baseline, candidate)| baseline as i64 - candidate as i64);
    let baseline_occurrences = before.map(|item| item.occurrences).unwrap_or_default();
    let candidate_occurrences = after.map(|item| item.occurrences).unwrap_or_default();
    let baseline_run_count = before.map(|item| item.run_count).unwrap_or_default();
    let candidate_run_count = after.map(|item| item.run_count).unwrap_or_default();

    FuzzHotspotCohortDelta {
        key,
        label: after
            .and_then(|item| item.label.clone())
            .or_else(|| before.and_then(|item| item.label.clone())),
        change_kind: change_kind(score_delta, before.is_some(), after.is_some()).to_string(),
        baseline_score,
        candidate_score,
        score_delta,
        relative_score_delta: score_delta,
        relative_lift,
        normalized_score_delta,
        baseline_rank: before.map(|item| item.rank),
        candidate_rank: after.map(|item| item.rank),
        rank_movement,
        baseline_occurrences,
        candidate_occurrences,
        occurrence_delta: candidate_occurrences as i64 - baseline_occurrences as i64,
        baseline_run_count,
        candidate_run_count,
        run_count_delta: candidate_run_count as i64 - baseline_run_count as i64,
    }
}

fn change_kind(score_delta: Option<f64>, has_baseline: bool, has_candidate: bool) -> &'static str {
    match (has_baseline, has_candidate, score_delta) {
        (false, true, _) => "new",
        (true, false, _) => "resolved",
        (true, true, Some(delta)) if delta > 0.0 => "increased",
        (true, true, Some(delta)) if delta < 0.0 => "decreased",
        _ => "unchanged",
    }
}
