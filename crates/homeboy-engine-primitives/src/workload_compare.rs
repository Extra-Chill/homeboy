//! One baseline / compare / gate engine for every workload kind (#15840).
//!
//! A kind supplies only a [`KindAdapter`]: how to snapshot its typed result,
//! how two snapshots differ, and what a difference means for the gate. The
//! rest lives here once:
//!
//! - the `homeboy.json` baseline slot (`baselines.<kind>` or
//!   `baselines.<kind>.rig.<rig-id>`), stored through the generic
//!   [`crate::baseline`] primitive;
//! - the `--baseline` / `--ignore-baseline` / `--ratchet` lifecycle
//!   ([`apply_baseline_flags`]);
//! - the pairwise comparison behind `homeboy fuzz compare` and
//!   `homeboy runs compare --kind <kind> --from-run .. --to-run ..`
//!   ([`compare`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

use homeboy_error::Result;

use crate::baseline::{self as store, Baseline, BaselineConfig, BaselineFlags};

/// What a delta means for the gate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Blocking regressions. Any entry fails the gate.
    pub regressions: Vec<String>,
    /// Regressions reported without failing the gate.
    pub advisories: Vec<String>,
    /// Improvements. Any entry lets `--ratchet` move the baseline.
    pub improvements: Vec<String>,
}

impl Verdict {
    pub fn regressed(&self) -> bool {
        !self.regressions.is_empty()
    }

    pub fn improved(&self) -> bool {
        !self.improvements.is_empty()
    }

    /// `worse` when anything regressed, else `better` when anything improved,
    /// else `same`.
    pub fn status(&self) -> &'static str {
        if self.regressed() {
            "worse"
        } else if self.improved() {
            "better"
        } else {
            "same"
        }
    }
}

/// Everything a workload kind contributes to baseline, compare and gate.
pub trait KindAdapter {
    /// Run kind, also the `homeboy.json` baseline key (`bench`, `trace`, ...).
    const KIND: &'static str;
    /// The typed result a snapshot is taken from.
    type Source: ?Sized;
    /// The comparable projection of one result. For baselined kinds this is
    /// also exactly what `homeboy.json` stores as the baseline metadata.
    type Snapshot;
    /// Kind-specific compare knobs (thresholds, policies, metric filters).
    type Options: ?Sized;
    /// The kind's comparison payload.
    type Delta;

    fn snapshot(source: &Self::Source) -> Self::Snapshot;

    fn delta(
        baseline: &Self::Snapshot,
        candidate: &Self::Snapshot,
        options: &Self::Options,
    ) -> Self::Delta;

    fn gate(delta: &Self::Delta) -> Verdict;

    /// Identity of the items a snapshot covers, recorded as the baseline's
    /// `known_fingerprints`. Aggregate kinds (test counts) have none.
    fn fingerprints(_snapshot: &Self::Snapshot) -> Vec<String> {
        Vec::new()
    }

    /// Refuse to compare snapshots that do not describe the same workload.
    fn comparable(_baseline: &Self::Snapshot, _candidate: &Self::Snapshot) -> Result<()> {
        Ok(())
    }
}

/// Items matched by key across a baseline and a candidate snapshot.
pub struct Paired<'a, T> {
    /// `(baseline, candidate)` for every candidate item the baseline also
    /// has, in candidate order.
    pub matched: Vec<(&'a T, &'a T)>,
    /// Keys only the candidate has, in candidate order.
    pub added: Vec<String>,
    /// Keys only the baseline has, in baseline order.
    pub removed: Vec<String>,
}

/// Pair snapshot items by key, the shared shape of every per-item delta.
pub fn pair_by_key<'a, T>(
    baseline: &'a [T],
    candidate: &'a [T],
    key: impl Fn(&T) -> &str,
) -> Paired<'a, T> {
    let baseline_by_key: HashMap<&str, &T> = baseline.iter().map(|b| (key(b), b)).collect();
    let candidate_keys: HashSet<&str> = candidate.iter().map(&key).collect();
    let mut paired = Paired {
        matched: Vec::new(),
        added: Vec::new(),
        removed: Vec::new(),
    };
    for item in candidate {
        match baseline_by_key.get(key(item)) {
            Some(prior) => paired.matched.push((*prior, item)),
            None => paired.added.push(key(item).to_string()),
        }
    }
    paired.removed = baseline
        .iter()
        .filter(|prior| !candidate_keys.contains(key(prior)))
        .map(|prior| key(prior).to_string())
        .collect();
    paired
}

/// A finished pairwise comparison.
pub struct Compared<A: KindAdapter> {
    pub baseline: A::Snapshot,
    pub candidate: A::Snapshot,
    pub delta: A::Delta,
    pub verdict: Verdict,
}

/// Compare two results of the same kind.
pub fn compare<A: KindAdapter>(
    baseline: &A::Source,
    candidate: &A::Source,
    options: &A::Options,
) -> Result<Compared<A>> {
    let baseline = A::snapshot(baseline);
    let candidate = A::snapshot(candidate);
    A::comparable(&baseline, &candidate)?;
    let delta = A::delta(&baseline, &candidate, options);
    let verdict = A::gate(&delta);
    Ok(Compared {
        baseline,
        candidate,
        delta,
        verdict,
    })
}

/// Where one kind's baseline lives inside a component's `homeboy.json`.
#[derive(Debug, Clone, Copy)]
pub struct BaselineSlot<'a> {
    pub root: &'a Path,
    /// Rig pin. Rig-pinned baselines live beside the unpinned one under
    /// `<kind>.rig.<rig-id>` so different rigs keep separate histories.
    pub rig_id: Option<&'a str>,
    /// Git ref whose committed `homeboy.json` is read when the working tree
    /// has no baseline.
    pub fallback_ref: Option<&'a str>,
}

impl<'a> BaselineSlot<'a> {
    pub fn new(root: &'a Path, rig_id: Option<&'a str>) -> Self {
        Self {
            root,
            rig_id,
            fallback_ref: None,
        }
    }

    pub fn with_fallback_ref(mut self, git_ref: Option<&'a str>) -> Self {
        self.fallback_ref = git_ref;
        self
    }

    fn key(&self, kind: &str) -> String {
        match self.rig_id {
            None => kind.to_string(),
            Some(rig_id) => format!("{kind}.rig.{rig_id}"),
        }
    }
}

/// Persist `source` as the kind's baseline.
pub fn save_baseline<A>(
    slot: &BaselineSlot,
    context_id: &str,
    source: &A::Source,
) -> Result<PathBuf>
where
    A: KindAdapter,
    A::Snapshot: Serialize + DeserializeOwned,
{
    let snapshot = A::snapshot(source);
    let config = BaselineConfig::new(slot.root, slot.key(A::KIND));
    store::save(&config, context_id, A::fingerprints(&snapshot), snapshot)
}

/// Load the kind's baseline. An unreadable baseline reads as absent.
pub fn load_baseline<A>(slot: &BaselineSlot) -> Option<Baseline<A::Snapshot>>
where
    A: KindAdapter,
    A::Snapshot: Serialize + DeserializeOwned,
{
    let key = slot.key(A::KIND);
    store::load(&BaselineConfig::new(slot.root, &key))
        .ok()
        .flatten()
        .or_else(|| {
            store::load_from_git_ref(&slot.root.to_string_lossy(), slot.fallback_ref?, &key)
        })
}

/// Outcome of the baseline lifecycle for one run.
#[derive(Debug, Clone)]
pub struct BaselineGate<D> {
    /// The comparison against the stored baseline, when one was made.
    pub comparison: Option<D>,
    /// `Some(1)` when the comparison regressed.
    pub exit_override: Option<i32>,
}

/// Run the `--baseline` / `--ignore-baseline` / `--ratchet` lifecycle.
///
/// `source` is `None` when the run produced nothing baselineable; nothing is
/// saved or compared then. `save_allowed` is the kind's precondition for
/// recording a new baseline with `--baseline` (gates passed, run passed).
///
/// - `--baseline`: save, never compare.
/// - `--ignore-baseline`: neither.
/// - otherwise: compare with the stored baseline; a regression sets the exit
///   override, and with `--ratchet` an improvement re-saves the baseline.
pub fn apply_baseline_flags<A>(
    slot: &BaselineSlot,
    context_id: &str,
    flags: &BaselineFlags,
    source: Option<&A::Source>,
    save_allowed: bool,
    options: &A::Options,
) -> Result<BaselineGate<A::Delta>>
where
    A: KindAdapter,
    A::Snapshot: Serialize + DeserializeOwned,
{
    let mut gate = BaselineGate {
        comparison: None,
        exit_override: None,
    };
    let Some(source) = source else {
        return Ok(gate);
    };
    if flags.baseline {
        if save_allowed {
            save_baseline::<A>(slot, context_id, source)?;
        }
        return Ok(gate);
    }
    if flags.ignore_baseline {
        return Ok(gate);
    }
    let Some(existing) = load_baseline::<A>(slot) else {
        return Ok(gate);
    };

    let delta = A::delta(&existing.metadata, &A::snapshot(source), options);
    let verdict = A::gate(&delta);
    if verdict.regressed() {
        gate.exit_override = Some(1);
    } else if verdict.improved() && flags.ratchet {
        // A failed ratchet write keeps the old baseline; the run still passed.
        let _ = save_baseline::<A>(slot, context_id, source);
    }
    gate.comparison = Some(delta);
    Ok(gate)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    /// A counter kind: one item per id, regresses when a value grows.
    struct Counter;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Values(Vec<(String, u64)>);

    impl KindAdapter for Counter {
        const KIND: &'static str = "counter";
        type Source = Values;
        type Snapshot = Values;
        type Options = ();
        type Delta = Vec<(String, i64)>;

        fn snapshot(source: &Values) -> Values {
            source.clone()
        }

        fn delta(baseline: &Values, candidate: &Values, _: &()) -> Self::Delta {
            candidate
                .0
                .iter()
                .filter_map(|(id, value)| {
                    let prior = baseline.0.iter().find(|(prior, _)| prior == id)?.1;
                    Some((id.clone(), *value as i64 - prior as i64))
                })
                .collect()
        }

        fn gate(delta: &Self::Delta) -> Verdict {
            Verdict {
                regressions: delta
                    .iter()
                    .filter(|(_, change)| *change > 0)
                    .map(|(id, _)| id.clone())
                    .collect(),
                advisories: Vec::new(),
                improvements: delta
                    .iter()
                    .filter(|(_, change)| *change < 0)
                    .map(|(id, _)| id.clone())
                    .collect(),
            }
        }

        fn fingerprints(snapshot: &Values) -> Vec<String> {
            snapshot.0.iter().map(|(id, _)| id.clone()).collect()
        }
    }

    fn values(value: u64) -> Values {
        Values(vec![("a".to_string(), value)])
    }

    fn flags(baseline: bool, ignore_baseline: bool, ratchet: bool) -> BaselineFlags {
        BaselineFlags {
            baseline,
            ignore_baseline,
            ratchet,
        }
    }

    fn run(dir: &Path, flags: &BaselineFlags, value: u64) -> BaselineGate<Vec<(String, i64)>> {
        let slot = BaselineSlot::new(dir, None);
        apply_baseline_flags::<Counter>(&slot, "demo", flags, Some(&values(value)), true, &())
            .expect("baseline lifecycle")
    }

    fn stored(dir: &Path, rig_id: Option<&str>) -> Option<u64> {
        load_baseline::<Counter>(&BaselineSlot::new(dir, rig_id)).map(|b| b.metadata.0[0].1)
    }

    #[test]
    fn baseline_flag_saves_without_comparing() {
        let dir = tempfile::tempdir().unwrap();
        let gate = run(dir.path(), &flags(true, false, false), 10);

        assert!(gate.comparison.is_none());
        assert_eq!(stored(dir.path(), None), Some(10));
        let saved = load_baseline::<Counter>(&BaselineSlot::new(dir.path(), None)).unwrap();
        assert_eq!(saved.context_id, "demo");
        assert_eq!(saved.known_fingerprints, vec!["a".to_string()]);
    }

    #[test]
    fn regression_sets_exit_override_and_keeps_baseline() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &flags(true, false, false), 10);

        let gate = run(dir.path(), &flags(false, false, true), 12);

        assert_eq!(gate.exit_override, Some(1));
        assert_eq!(gate.comparison, Some(vec![("a".to_string(), 2)]));
        assert_eq!(stored(dir.path(), None), Some(10));
    }

    #[test]
    fn improvement_ratchets_only_with_ratchet_flag() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &flags(true, false, false), 10);

        let gate = run(dir.path(), &flags(false, false, false), 8);
        assert_eq!(gate.exit_override, None);
        assert_eq!(stored(dir.path(), None), Some(10));

        run(dir.path(), &flags(false, false, true), 8);
        assert_eq!(stored(dir.path(), None), Some(8));
    }

    #[test]
    fn ignore_baseline_neither_saves_nor_compares() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &flags(true, false, false), 10);

        let gate = run(dir.path(), &flags(false, true, true), 99);

        assert!(gate.comparison.is_none());
        assert_eq!(gate.exit_override, None);
        assert_eq!(stored(dir.path(), None), Some(10));
    }

    #[test]
    fn save_requires_permission_and_a_source() {
        let dir = tempfile::tempdir().unwrap();
        let slot = BaselineSlot::new(dir.path(), None);
        let save = flags(true, false, false);
        apply_baseline_flags::<Counter>(&slot, "demo", &save, Some(&values(1)), false, &())
            .unwrap();
        apply_baseline_flags::<Counter>(&slot, "demo", &save, None, true, &()).unwrap();

        assert_eq!(stored(dir.path(), None), None);
    }

    #[test]
    fn rig_pinned_slot_is_isolated_from_unpinned() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline::<Counter>(&BaselineSlot::new(dir.path(), None), "demo", &values(1)).unwrap();
        save_baseline::<Counter>(
            &BaselineSlot::new(dir.path(), Some("studio")),
            "demo",
            &values(2),
        )
        .unwrap();

        assert_eq!(stored(dir.path(), None), Some(1));
        assert_eq!(stored(dir.path(), Some("studio")), Some(2));
        assert_eq!(stored(dir.path(), Some("other")), None);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("homeboy.json")).unwrap(),
        )
        .unwrap();
        assert!(json["baselines"]["counter.rig.studio"].is_object());
    }

    #[test]
    fn compare_snapshots_both_sides_and_reports_status() {
        let compared = compare::<Counter>(&values(10), &values(7), &()).unwrap();

        assert_eq!(compared.delta, vec![("a".to_string(), -3)]);
        assert_eq!(compared.verdict.status(), "better");
        assert_eq!(compared.baseline, values(10));
        assert_eq!(compared.candidate, values(7));
    }

    #[test]
    fn pair_by_key_matches_in_candidate_order_and_lists_one_sided_keys() {
        let baseline = ["a", "b", "c"];
        let candidate = ["c", "d", "a"];
        let paired = pair_by_key(&baseline, &candidate, |id| *id);

        let matched: Vec<_> = paired.matched.iter().map(|(b, c)| (**b, **c)).collect();
        assert_eq!(matched, vec![("c", "c"), ("a", "a")]);
        assert_eq!(paired.added, vec!["d".to_string()]);
        assert_eq!(paired.removed, vec!["b".to_string()]);
    }

    #[test]
    fn verdict_status_prefers_worse_over_better() {
        let verdict = Verdict {
            regressions: vec!["r".to_string()],
            advisories: Vec::new(),
            improvements: vec!["i".to_string()],
        };
        assert_eq!(verdict.status(), "worse");
        assert_eq!(Verdict::default().status(), "same");
    }
}
