//! Coverage is the union of clipped span intervals, never the sum of durations.
use super::ObservationStore;
use crate::{Error, Result};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseCoverage {
    pub run_id: String,
    pub wall_ms: f64,
    pub covered_ms: f64,
    pub covered_percentage: f64,
    pub gaps: Vec<PhaseCoverageGap>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseCoverageGap {
    pub started_at: String,
    pub duration_ms: f64,
    pub preceding_span: Option<String>,
    pub following_span: Option<String>,
}

impl ObservationStore {
    /// Read only durable evidence, including runs linked by `mission_id`.
    /// All spans are clipped to this run's timeline; overlaps count once.
    pub fn phase_coverage(&self, run_id: &str, limit: usize) -> Result<PhaseCoverage> {
        let run = self
            .get_run(run_id)?
            .ok_or_else(|| Error::internal_unexpected("run not found"))?;
        let parse = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.timestamp_millis() as f64)
                .map_err(|e| Error::internal_unexpected(e.to_string()))
        };
        let start = parse(&run.started_at)?;
        let end = run
            .finished_at
            .as_deref()
            .map(parse)
            .transpose()?
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() as f64)
            .max(start);
        let mut run_ids = vec![run_id.to_owned()];
        let mut after = None;
        loop {
            let page = self.list_mission_runs_page(run_id, after.as_ref(), 1000)?;
            run_ids.extend(
                page.runs
                    .into_iter()
                    .map(|run| run.id)
                    .filter(|id| id != run_id),
            );
            after = page.next_cursor;
            if after.is_none() {
                break;
            }
        }
        let mut intervals = Vec::new();
        for id in run_ids {
            for span in self.phase_spans(&id)?.spans {
                if let Some(duration) = span.duration_ms.filter(|d| d.is_finite() && *d > 0.0) {
                    let from = parse(&span.started_at)?;
                    let to = (from + duration).min(end);
                    let from = from.max(start);
                    if to > from {
                        intervals.push((from, to, span.id));
                    }
                }
            }
        }
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cursor = start;
        let mut previous = None;
        let mut covered = 0.0;
        let mut gaps = Vec::new();
        let gap = |from: f64, to: f64, preceding_span, following_span| PhaseCoverageGap {
            started_at: chrono::DateTime::from_timestamp_millis(from as i64)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            duration_ms: to - from,
            preceding_span,
            following_span,
        };
        for (from, to, id) in intervals {
            if from > cursor {
                gaps.push(gap(cursor, from, previous.clone(), Some(id.clone())));
            }
            if to > cursor {
                covered += to - cursor.max(from);
                cursor = to;
                previous = Some(id);
            }
        }
        if cursor < end {
            gaps.push(gap(cursor, end, previous, None));
        }
        gaps.sort_by(|a, b| b.duration_ms.total_cmp(&a.duration_ms));
        gaps.truncate(limit);
        let wall = end - start;
        Ok(PhaseCoverage {
            run_id: run_id.into(),
            wall_ms: wall,
            covered_ms: covered,
            covered_percentage: if wall > 0.0 {
                covered / wall * 100.0
            } else {
                100.0
            },
            gaps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::NewRunRecord;
    #[test]
    fn observation_coverage_clips_unions_and_names_largest_gaps() {
        use homeboy_engine_primitives::phase_timing::PhaseTimer;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("store.sqlite")).unwrap();
        let run = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .unwrap();
        let origin = chrono::DateTime::parse_from_rfc3339(&run.started_at).unwrap();
        let mut timer = PhaseTimer::new();
        timer.record_ok("first", Duration::from_millis(10));
        timer.record_failed("nested", Duration::from_millis(2));
        timer.record_ok("last", Duration::from_millis(10));
        let mut report = timer.into_report();
        for (span, offset) in report.spans.iter_mut().zip([0, 3, 20]) {
            span.started_at = (origin + chrono::Duration::milliseconds(offset))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        }
        store
            .record_phase_spans(&run.id, "coverage", &report)
            .unwrap();
        // Use a future-free live window with a substantial trailing gap.
        std::thread::sleep(Duration::from_millis(60));
        let coverage = store.phase_coverage(&run.id, 10).unwrap();
        assert_eq!(coverage.covered_ms, 20.0);
        let middle = coverage
            .gaps
            .iter()
            .find(|gap| gap.preceding_span.as_deref() == Some("first"))
            .unwrap();
        assert_eq!(middle.duration_ms, 10.0);
        assert_eq!(middle.following_span.as_deref(), Some("last"));
        assert!(coverage
            .gaps
            .windows(2)
            .all(|pair| pair[0].duration_ms >= pair[1].duration_ms));
        assert!(coverage.covered_percentage < 50.0);
    }

    #[test]
    fn observation_mission_coverage_unions_attempts_and_preserves_idle_gap() {
        use homeboy_engine_primitives::phase_timing::PhaseTimer;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("store.sqlite")).unwrap();
        let mut mission = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .unwrap();
        let origin = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap();
        let at = |ms| {
            (origin + chrono::Duration::milliseconds(ms))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        mission.started_at = at(0);
        mission.finished_at = Some(at(100));
        store.upsert_imported_run(&mission).unwrap();

        let mut attempts = Vec::new();
        for (start, end, name) in [(10, 40, "attempt.one"), (60, 100, "attempt.two")] {
            let mut attempt = store
                .start_run_in_mission(NewRunRecord::builder("agent-task").build(), &mission.id)
                .unwrap();
            attempt.started_at = at(start);
            attempt.finished_at = Some(at(end));
            store.upsert_imported_run(&attempt).unwrap();
            let mut timer = PhaseTimer::new();
            timer.record_ok(name, Duration::from_millis((end - start) as u64));
            let mut report = timer.into_report();
            report.spans[0].started_at = at(start);
            store
                .record_phase_spans(&attempt.id, name, &report)
                .unwrap();
            attempts.push(attempt);
        }
        // The parent's span overlaps attempt one and extends before its own start.
        let mut timer = PhaseTimer::new();
        timer.record_ok("controller.job", Duration::from_millis(30));
        let mut report = timer.into_report();
        report.spans[0].started_at = at(-10);
        store
            .record_phase_spans(&mission.id, "parent", &report)
            .unwrap();
        // Unrelated runs must not fill the idle gap.
        let unrelated = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .unwrap();
        report.spans[0].started_at = at(40);
        store
            .record_phase_spans(&unrelated.id, "unrelated", &report)
            .unwrap();

        let coverage = store.phase_coverage(&mission.id, 10).unwrap();
        assert_eq!(coverage.wall_ms, 100.0);
        assert_eq!(coverage.covered_ms, 80.0);
        assert_eq!(coverage.covered_percentage, 80.0);
        assert_eq!(
            coverage.gaps,
            vec![PhaseCoverageGap {
                started_at: at(40),
                duration_ms: 20.0,
                preceding_span: Some("attempt.one".into()),
                following_span: Some("attempt.two".into()),
            }]
        );
        for attempt in &attempts {
            let coverage = store.phase_coverage(&attempt.id, 10).unwrap();
            assert_eq!(coverage.covered_ms, coverage.wall_ms);
            assert_eq!(coverage.covered_percentage, 100.0);
            assert!(coverage.gaps.is_empty());
            assert_eq!(store.phase_spans(&attempt.id).unwrap().spans.len(), 1);
        }
        assert_eq!(store.phase_spans(&mission.id).unwrap().spans.len(), 1);
    }

    #[test]
    fn observation_phase_persistence_failure_preserves_domain_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("store.sqlite")).unwrap();
        // No such run: the append fails, but successful work still succeeds.
        assert_eq!(
            store
                .time_run_phase("missing-run", "terminal.record", || Ok(42))
                .unwrap(),
            42
        );
        let result: Result<()> = store.time_run_phase("missing-run", "terminal.record", || {
            Err(Error::internal_unexpected("original failure"))
        });
        assert!(result.unwrap_err().message.contains("original failure"));
    }

    #[test]
    fn observation_coverage_reads_nested_failed_work_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("store.sqlite")).unwrap();
        let run = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .unwrap();
        let result: Result<()> = store.time_run_phase(&run.id, "promote", || {
            store.time_run_phase(&run.id, "gate", || {
                std::thread::sleep(std::time::Duration::from_millis(10));
                Err(Error::internal_unexpected("gate failure"))
            })
        });
        assert!(result.is_err());
        let coverage = store.phase_coverage(&run.id, 10).unwrap();
        assert!(coverage.covered_ms >= 9.0);
        assert!(coverage.covered_ms <= coverage.wall_ms);
        assert_eq!(store.phase_spans(&run.id).unwrap().spans.len(), 2);
    }
}
