//! Test duration capture and the slow-test policy.
//!
//! Homeboy has always been able to say a test *failed*. This module makes it
//! able to say a test is *slow*, from the same evidence it already captures:
//! the test child's stdout plus the wall clock. Nothing new has to be run.
//!
//! # Where the numbers come from
//!
//! Per-binary durations are read from the runner's own summary lines
//! (`test result: ... finished in 470.21s` for libtest, `Summary [ 12.3s]` for
//! nextest, `Time:` for PHPUnit). Per-test durations are read when the runner
//! reports them — nextest always does, libtest only under `--report-time` —
//! and are otherwise inferred *exactly* for the common single-test binary,
//! where the binary duration and the test duration are the same measurement.
//!
//! An optional `test.durations` sidecar is consulted first, so an extension can
//! supply richer timings later without any change here. The stdout path is the
//! fallback that makes this work today with no extension release.
//!
//! # Why share-of-budget, not an absolute number
//!
//! A 400-second test is unremarkable in a two-hour suite and pathological in a
//! twenty-five-minute one, so an absolute threshold is wrong somewhere by
//! construction. Of the two relative denominators available, this module keys
//! the rule on **share of the enforced budget**, not share of suite duration:
//!
//! * The budget is the resource actually under contention. Exhausting it is
//!   what turns the gate red, so a share of it is a direct measure of risk.
//! * It is attributable. A test's share of the budget changes only when that
//!   test changes or the budget changes. Share of *suite* moves whenever any
//!   other test changes — a test can cross the threshold because somebody
//!   optimised something unrelated, which is a finding nobody can act on.
//! * It degrades correctly on a fast suite. The slowest test in a sixty-second
//!   suite is half the suite but a rounding error against the budget, and
//!   flagging it would be noise.
//!
//! Share of suite is still *reported* on every unit, because it is the right
//! number for "where did the time go". It just does not drive the flag.
//!
//! A uniformly slow suite has no single outlier and therefore trips no
//! slow-unit finding — correctly, since no one test is the problem. That case
//! is a separate rule, [`slow_rule::SUITE_NEAR_BUDGET`], because it has a
//! different fix.

use std::time::Duration;

pub use homeboy_extension_contract::test_duration::{
    duration_source, slow_rule, SlowTestFinding, TestDurations, TestUnitDuration,
};

use homeboy_core::error::Result;
use homeboy_engine_primitives::local_files;

/// Default share of the enforced budget above which one test unit is a
/// finding. At the 1500 s budget this repository ran until recently, 10% is
/// 150 s; the binary that provoked issue #10655 measured 470 s (31%) while the
/// next slowest measured 78 s (5%).
pub const DEFAULT_SLOW_UNIT_BUDGET_SHARE: f64 = 0.10;

/// Absolute floor, in seconds, below which no unit is flagged regardless of
/// share. Stops a trivially short suite under a generous budget from
/// generating findings about tests that cost nothing.
pub const DEFAULT_SLOW_UNIT_FLOOR_SECONDS: f64 = 30.0;

/// Default share of the budget at which the suite as a whole is reported as
/// approaching termination. This is the signal that predicts a timeout before
/// it happens.
pub const DEFAULT_SUITE_BUDGET_SHARE: f64 = 0.75;

/// How many units the report surfaces.
pub const SLOWEST_UNITS_REPORTED: usize = 10;

const SLOW_UNIT_SHARE_ENV: &str = "HOMEBOY_SLOW_TEST_BUDGET_SHARE";
const SLOW_UNIT_FLOOR_ENV: &str = "HOMEBOY_SLOW_TEST_FLOOR_SECONDS";
const SUITE_SHARE_ENV: &str = "HOMEBOY_SLOW_SUITE_BUDGET_SHARE";

/// Thresholds applied to a duration sample set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlowTestPolicy {
    pub budget_seconds: Option<f64>,
    pub unit_budget_share: f64,
    pub unit_floor_seconds: f64,
    pub suite_budget_share: f64,
}

impl SlowTestPolicy {
    /// Build the policy for a run armed with `budget`, honouring environment
    /// overrides. An override that does not parse, or that is out of range, is
    /// ignored in favour of the default rather than silently disabling the
    /// rule.
    pub fn for_budget(budget: Option<Duration>) -> Self {
        Self {
            budget_seconds: budget.map(|budget| budget.as_secs_f64()).filter(positive),
            unit_budget_share: env_share(SLOW_UNIT_SHARE_ENV, DEFAULT_SLOW_UNIT_BUDGET_SHARE),
            unit_floor_seconds: env_seconds(SLOW_UNIT_FLOOR_ENV, DEFAULT_SLOW_UNIT_FLOOR_SECONDS),
            suite_budget_share: env_share(SUITE_SHARE_ENV, DEFAULT_SUITE_BUDGET_SHARE),
        }
    }
}

fn positive(value: &f64) -> bool {
    value.is_finite() && *value > 0.0
}

fn env_share(key: &str, fallback: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|share| share.is_finite() && *share > 0.0 && *share <= 1.0)
        .unwrap_or(fallback)
}

fn env_seconds(key: &str, fallback: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .unwrap_or(fallback)
}

/// Raw duration samples, before any policy is applied.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TestDurationSamples {
    pub binaries: Vec<TestUnitDuration>,
    pub tests: Vec<TestUnitDuration>,
    /// A suite-level total the runner reported directly (nextest `Summary`,
    /// PHPUnit `Time:`), used when no per-binary breakdown is available.
    pub suite_seconds: Option<f64>,
    /// A binary that started and never reported a result. Present when the
    /// child was killed mid-run.
    pub unfinished: Vec<TestUnitDuration>,
}

impl TestDurationSamples {
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
            && self.tests.is_empty()
            && self.unfinished.is_empty()
            && self.suite_seconds.is_none()
    }
}

/// Read the optional `test.durations` sidecar.
///
/// The sidecar is an *inbound* contract: extensions that can produce richer
/// timings than stdout carries (per-test times from a JSON test reporter, for
/// example) write it, and this is the consumer. Absent or unparseable means
/// "no extra information", never an error — duration is advisory and must not
/// be able to fail a run in either direction.
pub fn parse_test_durations_file(path: &std::path::Path) -> Result<Option<TestDurations>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = local_files::read_file(path, "read test durations file")?;
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Ok(None);
    };
    if homeboy_core::structured_sidecar::validate_payload("test.durations", &payload).is_err() {
        return Ok(None);
    }
    Ok(serde_json::from_value::<TestDurations>(payload).ok())
}

/// Parse duration samples out of a test runner's captured output.
pub fn parse_duration_samples(text: &str) -> TestDurationSamples {
    let mut samples = TestDurationSamples::default();
    let mut current: Option<BinaryAccumulator> = None;

    for line in text.lines() {
        let trimmed = line.trim_end();

        if let Some(unit) = parse_running_line(trimmed) {
            if let Some(previous) = current.take() {
                samples.unfinished.push(previous.into_unfinished());
            }
            current = Some(BinaryAccumulator::new(unit));
            continue;
        }

        if let Some((seconds, tests)) = parse_binary_result_line(trimmed) {
            if let Some(accumulator) = current.take() {
                let (binary, mut per_test) = accumulator.finish(seconds, tests);
                samples.binaries.push(binary);
                samples.tests.append(&mut per_test);
            }
            continue;
        }

        if let Some(seconds) = parse_nextest_summary_line(trimmed) {
            samples.suite_seconds = Some(seconds);
            continue;
        }

        if let Some(seconds) = parse_phpunit_time_line(trimmed) {
            samples.suite_seconds = Some(seconds);
            continue;
        }

        if let Some(unit) = parse_nextest_case_line(trimmed) {
            samples.tests.push(unit);
            continue;
        }

        if let Some(accumulator) = current.as_mut() {
            accumulator.observe(trimmed);
        }
    }

    if let Some(previous) = current.take() {
        samples.unfinished.push(previous.into_unfinished());
    }

    samples
}

/// Apply the slow-test policy to a sample set and produce the reportable
/// duration block.
///
/// `phase_seconds` is Homeboy's own wall-clock measurement of the child, which
/// is available even when nothing parsed. `complete` is false when the child
/// was terminated; the findings still ship, labelled, because a killed run is
/// exactly when knowing what ate the budget matters most.
pub fn build_test_durations(
    samples: TestDurationSamples,
    phase_seconds: Option<f64>,
    policy: SlowTestPolicy,
    incomplete_reason: Option<String>,
) -> TestDurations {
    let mut durations = TestDurations {
        phase_seconds: phase_seconds.filter(positive),
        budget_seconds: policy.budget_seconds,
        complete: incomplete_reason.is_none(),
        incomplete_reason,
        ..TestDurations::default()
    };

    let measured: f64 = samples
        .binaries
        .iter()
        .filter_map(|unit| unit.seconds)
        .sum();
    // Never report a measured total of zero when nothing was measured: that
    // reads as "the suite ran instantly" rather than "we do not know".
    durations.measured_seconds = if measured > 0.0 {
        Some(round_seconds(measured))
    } else {
        samples.suite_seconds.filter(positive)
    };

    let mut binaries = samples.binaries;
    let mut tests = samples.tests;
    for unit in binaries.iter_mut().chain(tests.iter_mut()) {
        annotate_shares(unit, policy.budget_seconds, durations.measured_seconds);
    }

    let mut unfinished = samples.unfinished;
    for unit in unfinished.iter_mut() {
        annotate_shares(unit, policy.budget_seconds, durations.measured_seconds);
    }

    let slow = evaluate_slow(
        &binaries,
        &tests,
        &unfinished,
        &durations,
        policy,
        !durations.complete,
    );
    durations.slow = slow;

    sort_by_duration(&mut binaries);
    sort_by_duration(&mut tests);
    // Unfinished units carry no duration, so they sort last but must still be
    // visible: they are the units a timeout hid.
    binaries.extend(unfinished);

    durations.binaries = binaries;
    durations.tests = tests;
    durations
}

fn annotate_shares(unit: &mut TestUnitDuration, budget: Option<f64>, measured: Option<f64>) {
    let Some(seconds) = unit.seconds else {
        return;
    };
    unit.budget_share = budget
        .filter(positive)
        .map(|budget| round_share(seconds / budget));
    unit.measured_share = measured
        .filter(positive)
        .map(|measured| round_share(seconds / measured));
}

/// Choose the units the slow rule is evaluated over, so nothing is counted
/// twice: per-test samples supersede their own binary when present, because
/// they localise the cost further.
fn evaluation_units(
    binaries: &[TestUnitDuration],
    tests: &[TestUnitDuration],
) -> Vec<TestUnitDuration> {
    let mut units = Vec::new();
    for binary in binaries {
        let owned: Vec<&TestUnitDuration> = tests
            .iter()
            .filter(|test| test.binary.as_deref() == Some(binary.name.as_str()))
            .collect();
        if owned.is_empty() {
            units.push(binary.clone());
        } else {
            units.extend(owned.into_iter().cloned());
        }
    }
    // Per-test samples with no matching binary (nextest reports the binary by
    // crate name, not target path) still deserve evaluation.
    for test in tests {
        let orphan = !binaries
            .iter()
            .any(|binary| Some(binary.name.as_str()) == test.binary.as_deref());
        if orphan {
            units.push(test.clone());
        }
    }
    units
}

fn evaluate_slow(
    binaries: &[TestUnitDuration],
    tests: &[TestUnitDuration],
    unfinished: &[TestUnitDuration],
    durations: &TestDurations,
    policy: SlowTestPolicy,
    incomplete: bool,
) -> Vec<SlowTestFinding> {
    let mut findings = Vec::new();

    let mut units = evaluation_units(binaries, tests);
    sort_by_duration(&mut units);

    for unit in units {
        let Some(seconds) = unit.seconds else {
            continue;
        };
        if seconds < policy.unit_floor_seconds {
            continue;
        }
        let Some(share) = unit.budget_share else {
            // Without a budget there is no honest relative threshold, so the
            // unit is reported but not flagged.
            continue;
        };
        if share < policy.unit_budget_share {
            continue;
        }
        findings.push(SlowTestFinding {
            rule: slow_rule::SLOW_UNIT.to_string(),
            message: format!(
                "`{}` ran {:.1}s — {:.0}% of the {:.0}s test budget{}",
                unit.name,
                seconds,
                share * 100.0,
                policy.budget_seconds.unwrap_or_default(),
                unit.measured_share
                    .map(|measured| format!(", {:.0}% of measured suite time", measured * 100.0))
                    .unwrap_or_default()
            ),
            name: unit.name.clone(),
            binary: unit.binary.clone(),
            seconds: Some(round_seconds(seconds)),
            min_seconds: None,
            budget_share: unit.budget_share,
            measured_share: unit.measured_share,
            incomplete,
        });
    }

    for unit in unfinished {
        findings.push(SlowTestFinding {
            rule: slow_rule::UNFINISHED_UNIT.to_string(),
            message: match unit.min_seconds {
                Some(bound) => format!(
                    "`{}` was still running after at least {:.0}s and never reported a duration",
                    unit.name, bound
                ),
                None => format!(
                    "`{}` started and never reported a duration; its cost is unknown",
                    unit.name
                ),
            },
            name: unit.name.clone(),
            binary: unit.binary.clone(),
            seconds: None,
            min_seconds: unit.min_seconds,
            budget_share: None,
            measured_share: None,
            incomplete: true,
        });
    }

    if let Some(budget) = policy.budget_seconds.filter(positive) {
        let suite = durations
            .phase_seconds
            .into_iter()
            .chain(durations.measured_seconds)
            .fold(f64::NAN, f64::max);
        if suite.is_finite() {
            let share = suite / budget;
            if share >= policy.suite_budget_share {
                findings.push(SlowTestFinding {
                    rule: slow_rule::SUITE_NEAR_BUDGET.to_string(),
                    message: format!(
                        "test suite consumed {:.0}s of its {:.0}s budget ({:.0}%)",
                        suite,
                        budget,
                        share * 100.0
                    ),
                    name: "test suite".to_string(),
                    binary: None,
                    seconds: Some(round_seconds(suite)),
                    min_seconds: None,
                    budget_share: Some(round_share(share)),
                    measured_share: None,
                    incomplete,
                });
            }
        }
    }

    findings
}

fn sort_by_duration(units: &mut [TestUnitDuration]) {
    units.sort_by(|a, b| {
        b.seconds
            .partial_cmp(&a.seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn round_seconds(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn round_share(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

// ── line parsers ────────────────────────────────────────────────────────

struct BinaryAccumulator {
    unit: TestUnitDuration,
    case_names: Vec<String>,
    timed_cases: Vec<TestUnitDuration>,
    long_running: Vec<(String, f64)>,
}

impl BinaryAccumulator {
    fn new(unit: TestUnitDuration) -> Self {
        Self {
            unit,
            case_names: Vec::new(),
            timed_cases: Vec::new(),
            long_running: Vec::new(),
        }
    }

    fn observe(&mut self, line: &str) {
        if let Some((name, seconds)) = parse_long_running_line(line) {
            self.long_running.push((name, seconds));
            return;
        }
        if let Some((name, seconds)) = parse_libtest_case_line(line) {
            match seconds {
                Some(seconds) => {
                    let mut unit =
                        TestUnitDuration::new(name.clone(), duration_source::REPORT_TIME);
                    unit.binary = Some(self.unit.name.clone());
                    unit.seconds = Some(seconds);
                    self.timed_cases.push(unit);
                }
                None => self.case_names.push(name),
            }
        }
    }

    fn finish(
        mut self,
        seconds: f64,
        tests: Option<u64>,
    ) -> (TestUnitDuration, Vec<TestUnitDuration>) {
        self.unit.seconds = Some(seconds);
        self.unit.tests = tests;

        let mut cases = std::mem::take(&mut self.timed_cases);

        // Exactly one test in the binary: the binary's duration *is* that
        // test's duration. This is an identity, not an estimate, and it is the
        // case that matters — the binary behind #10655 held a single test.
        if cases.is_empty() && tests == Some(1) {
            if let Some(name) = self
                .case_names
                .first()
                .cloned()
                .or_else(|| self.long_running.first().map(|(name, _)| name.clone()))
            {
                let mut unit = TestUnitDuration::new(name, duration_source::SOLE_TEST);
                unit.binary = Some(self.unit.name.clone());
                unit.seconds = Some(seconds);
                cases.push(unit);
            }
        }

        (self.unit, cases)
    }

    /// The binary was still running when output stopped — a killed child. Its
    /// duration is unknown; a long-running notice, if libtest emitted one,
    /// gives a lower bound.
    fn into_unfinished(mut self) -> TestUnitDuration {
        self.unit.source = duration_source::LONG_RUNNING_NOTICE.to_string();
        self.unit.min_seconds = self
            .long_running
            .iter()
            .map(|(_, seconds)| *seconds)
            .fold(None::<f64>, |acc, seconds| {
                Some(acc.map_or(seconds, |acc: f64| acc.max(seconds)))
            });
        if let Some((name, _)) = self.long_running.first() {
            self.unit.name = format!("{} ({})", name, self.unit.name);
        }
        self.unit
    }
}

fn parse_running_line(line: &str) -> Option<TestUnitDuration> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("Running ") {
        // `Running <target> (<executable path>)`. The trailing parenthesised
        // path is what distinguishes a cargo target line from prose such as
        // "Running Rust tests...".
        let open = rest.rfind(" (")?;
        if !rest.ends_with(')') {
            return None;
        }
        let target = rest[..open].trim();
        if target.is_empty() {
            return None;
        }
        let executable = rest[open + 2..rest.len() - 1]
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        // Target paths are not unique across a workspace — every crate has an
        // `unittests src/lib.rs`, and two crates may both have `tests/foo.rs`.
        // Qualify the name with the owning crate so per-test samples attach to
        // the right binary and the report names something a reader can find.
        let name = match executable.as_deref().map(crate_name_from_executable) {
            Some(owner) if !owner.is_empty() => format!("{target} ({owner})"),
            _ => target.to_string(),
        };
        let mut unit = TestUnitDuration::new(name, duration_source::BINARY_SUMMARY);
        unit.binary = executable;
        return Some(unit);
    }
    if let Some(crate_name) = trimmed.strip_prefix("Doc-tests ") {
        let crate_name = crate_name.trim();
        if crate_name.is_empty() || crate_name.contains(' ') {
            return None;
        }
        return Some(TestUnitDuration::new(
            format!("Doc-tests {crate_name}"),
            duration_source::BINARY_SUMMARY,
        ));
    }
    None
}

/// `reverse_cook_queue_acceptance-fde0b7dcd4346e6e` → `reverse_cook_queue_acceptance`.
/// Cargo appends `-<hex>` to every test executable; the stem is the crate or
/// target name a human would recognise.
fn crate_name_from_executable(executable: &str) -> &str {
    match executable.rsplit_once('-') {
        Some((stem, hash))
            if !stem.is_empty()
                && hash.len() >= 8
                && hash.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            stem
        }
        _ => executable,
    }
}

fn parse_binary_result_line(line: &str) -> Option<(f64, Option<u64>)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("test result:") {
        return None;
    }
    let seconds = after_marker(trimmed, "finished in ")
        .and_then(|rest| rest.strip_suffix('s').unwrap_or(rest).trim().parse().ok())?;
    let tests = count_before(trimmed, "passed")
        .map(|passed| passed + count_before(trimmed, "failed").unwrap_or(0));
    Some((seconds, tests))
}

fn after_marker<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let index = line.find(marker)? + marker.len();
    let rest = &line[index..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 's'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn count_before(line: &str, label: &str) -> Option<u64> {
    let index = line.find(&format!(" {label}"))?;
    line[..index]
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|token| !token.is_empty())?
        .parse()
        .ok()
}

/// `test some::name ... ok` or, under `--report-time`, `test some::name ... ok <1.234s>`.
fn parse_libtest_case_line(line: &str) -> Option<(String, Option<f64>)> {
    let rest = line.trim_start().strip_prefix("test ")?;
    let marker = rest.find(" ... ")?;
    let name = rest[..marker].trim();
    if name.is_empty() {
        return None;
    }
    let outcome = rest[marker + 5..].trim();
    let seconds = outcome
        .find('<')
        .and_then(|open| outcome[open + 1..].strip_suffix('>'))
        .and_then(|value| value.strip_suffix('s'))
        .and_then(|value| value.parse().ok());
    Some((name.to_string(), seconds))
}

/// libtest's built-in notice. It names a slow test without timing it, so it
/// only ever yields a lower bound.
fn parse_long_running_line(line: &str) -> Option<(String, f64)> {
    let rest = line.trim_start().strip_prefix("test ")?;
    let marker = rest.find(" has been running for over ")?;
    let name = rest[..marker].trim();
    let tail = rest[marker + " has been running for over ".len()..].trim();
    let seconds = tail
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    (!name.is_empty()).then(|| (name.to_string(), seconds))
}

/// cargo-nextest: `    PASS [   0.019s] homeboy-core config::tests::name`.
fn parse_nextest_case_line(line: &str) -> Option<TestUnitDuration> {
    let trimmed = line.trim_start();
    let status_end = trimmed.find(" [")?;
    let status = trimmed[..status_end].trim();
    if !matches!(status, "PASS" | "FAIL" | "SLOW" | "LEAK" | "TIMEOUT") {
        return None;
    }
    let rest = &trimmed[status_end + 2..];
    let close = rest.find(']')?;
    let seconds: f64 = rest[..close].trim().trim_end_matches('s').parse().ok()?;
    let mut parts = rest[close + 1..].split_whitespace();
    let binary = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    let mut unit = TestUnitDuration::new(name, duration_source::NEXTEST);
    unit.binary = Some(binary);
    unit.seconds = Some(seconds);
    Some(unit)
}

/// cargo-nextest: `     Summary [  12.345s] 100 tests run: 100 passed`.
fn parse_nextest_summary_line(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("Summary [")?;
    let close = rest.find(']')?;
    rest[..close].trim().trim_end_matches('s').parse().ok()
}

/// PHPUnit: `Time: 00:01.234, Memory: 6.00 MB` or `Time: 1.23 seconds`.
fn parse_phpunit_time_line(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("Time: ")?;
    let value = rest.split(',').next()?.trim();
    if let Some(seconds) = value
        .strip_suffix(" seconds")
        .or(value.strip_suffix(" second"))
    {
        return seconds.trim().parse().ok();
    }
    // `00:01.234` (mm:ss) and `00:00:01.234` (hh:mm:ss) are both emitted
    // depending on PHPUnit version; fold left so either works.
    let mut total = 0.0;
    for part in value.split(':') {
        let part: f64 = part.trim().parse().ok()?;
        total = total * 60.0 + part;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim stdout of the cargo test phase from homeboy CI job 90359883772
    /// (run 30383598995, 2026-07-28), with only the GitHub log timestamp prefix
    /// removed. Recorded rather than hand-written so the classifier is proven
    /// against the exact bytes the gate really sees.
    const RECORDED_CARGO_OUTPUT: &str =
        include_str!("../../../../tests/fixtures/test_durations/cargo-test-slow-binary.txt");

    /// The same recording truncated at a real line boundary, mid-binary, the
    /// way a SIGKILL at the timeout truncates it: earlier binaries have
    /// reported, the one that was executing never will.
    const RECORDED_TRUNCATED_OUTPUT: &str =
        include_str!("../../../../tests/fixtures/test_durations/cargo-test-timeout-truncated.txt");

    /// The budget this repository's Test gate enforced when #10655 was filed.
    const BUDGET_SECONDS: f64 = 1500.0;

    fn policy(budget: Option<f64>) -> SlowTestPolicy {
        SlowTestPolicy {
            budget_seconds: budget,
            unit_budget_share: DEFAULT_SLOW_UNIT_BUDGET_SHARE,
            unit_floor_seconds: DEFAULT_SLOW_UNIT_FLOOR_SECONDS,
            suite_budget_share: DEFAULT_SUITE_BUDGET_SHARE,
        }
    }

    fn recorded(budget: Option<f64>) -> TestDurations {
        build_test_durations(
            parse_duration_samples(RECORDED_CARGO_OUTPUT),
            None,
            policy(budget),
            None,
        )
    }

    fn finding<'a>(durations: &'a TestDurations, rule: &str) -> Vec<&'a SlowTestFinding> {
        durations
            .slow
            .iter()
            .filter(|found| found.rule == rule)
            .collect()
    }

    #[test]
    fn recorded_ci_output_yields_per_binary_durations() {
        let samples = parse_duration_samples(RECORDED_CARGO_OUTPUT);

        assert!(
            samples.binaries.len() > 20,
            "recorded output holds many binaries, parsed {}",
            samples.binaries.len()
        );
        assert!(
            samples.unfinished.is_empty(),
            "a complete recording leaves nothing unfinished: {:?}",
            samples.unfinished
        );

        let slowest = samples
            .binaries
            .iter()
            .max_by(|a, b| a.seconds.partial_cmp(&b.seconds).unwrap())
            .expect("recorded output has binaries");
        assert!(
            slowest
                .name
                .starts_with("tests/reverse_cook_queue_acceptance.rs"),
            "slowest binary was {}",
            slowest.name
        );
        assert_eq!(slowest.seconds, Some(470.21));
        assert_eq!(slowest.tests, Some(1));
    }

    #[test]
    fn the_budget_hog_is_flagged_and_named_by_its_test() {
        let durations = recorded(Some(BUDGET_SECONDS));
        let slow = finding(&durations, slow_rule::SLOW_UNIT);

        assert_eq!(
            slow.len(),
            1,
            "exactly one unit crosses the share threshold: {:#?}",
            durations.slow
        );
        let found = slow[0];
        // A binary holding a single test reports the *test* name, because that
        // is what a reader has to go and fix.
        assert_eq!(
            found.name, "detached_cook_accepts_reverse_capacity_queue_and_worker_completes_once",
            "finding should name the sole test, not just its binary"
        );
        assert_eq!(found.seconds, Some(470.21));
        let share = found.budget_share.expect("share against a known budget");
        assert!(
            (share - 470.21 / BUDGET_SECONDS).abs() < 1e-3,
            "share was {share}"
        );
        assert!(!found.incomplete, "a complete run is not labelled partial");
    }

    #[test]
    fn the_second_slowest_binary_is_reported_but_not_flagged() {
        let durations = recorded(Some(BUDGET_SECONDS));

        let second = durations
            .binaries
            .iter()
            .find(|unit| unit.name.starts_with("tests/cook_lab_handoff_test.rs"))
            .expect("second slowest binary is reported");
        assert_eq!(second.seconds, Some(78.33));

        // 78 s clears the 30 s absolute floor and is the second largest cost in
        // the suite, yet it is only ~5% of the budget. Share, not rank and not
        // an absolute number, is what decides.
        assert!(
            !durations
                .slow
                .iter()
                .any(|found| found.name.contains("cook_lab_handoff")),
            "78s is 5% of a 1500s budget and must not be flagged: {:#?}",
            durations.slow
        );
    }

    #[test]
    fn share_of_suite_is_reported_but_does_not_drive_the_flag() {
        let durations = recorded(Some(BUDGET_SECONDS));
        let hog = durations
            .binaries
            .iter()
            .find(|unit| {
                unit.name
                    .starts_with("tests/reverse_cook_queue_acceptance.rs")
            })
            .expect("recorded hog");

        let measured_share = hog.measured_share.expect("suite share is reported");
        let budget_share = hog.budget_share.expect("budget share is reported");
        assert!(
            measured_share > 0.8,
            "the hog is most of the measured suite: {measured_share}"
        );
        assert!(
            budget_share < 0.4,
            "but only a third of the budget: {budget_share}"
        );
        assert!(
            measured_share > budget_share,
            "the two denominators genuinely differ on this recording"
        );
    }

    #[test]
    fn a_generous_budget_silences_the_same_recording() {
        // Identical evidence, four times the budget: 470 s is now 7.8%, under
        // the threshold. This is the whole argument for a relative rule — the
        // test did not change, its cost did not change, only what it costs
        // *us* changed.
        let durations = recorded(Some(BUDGET_SECONDS * 4.0));
        assert!(
            finding(&durations, slow_rule::SLOW_UNIT).is_empty(),
            "{:#?}",
            durations.slow
        );
        assert!(
            durations
                .binaries
                .iter()
                .any(|unit| unit.seconds == Some(470.21)),
            "the measurement is still reported even when it is not a finding"
        );
    }

    #[test]
    fn without_a_budget_nothing_is_flagged_but_everything_is_measured() {
        let durations = recorded(None);
        assert!(
            durations.slow.is_empty(),
            "no budget means no honest relative threshold: {:#?}",
            durations.slow
        );
        assert!(durations.measured_seconds.unwrap() > 500.0);
    }

    #[test]
    fn measured_total_is_absent_rather_than_zero_when_nothing_parsed() {
        let durations = build_test_durations(
            parse_duration_samples("no test output here at all\n"),
            None,
            policy(Some(BUDGET_SECONDS)),
            None,
        );
        assert_eq!(
            durations.measured_seconds, None,
            "an unmeasured suite is unknown, not instantaneous"
        );
        assert!(durations.binaries.is_empty());
        assert!(durations.slow.is_empty());
        assert!(durations.is_empty());
    }

    #[test]
    fn a_killed_run_keeps_partial_timings_and_labels_them_incomplete() {
        let durations = build_test_durations(
            parse_duration_samples(RECORDED_TRUNCATED_OUTPUT),
            Some(BUDGET_SECONDS),
            policy(Some(BUDGET_SECONDS)),
            Some("test child terminated at its budget".to_string()),
        );

        assert!(!durations.complete);
        assert_eq!(
            durations.incomplete_reason.as_deref(),
            Some("test child terminated at its budget")
        );
        assert!(
            durations
                .binaries
                .iter()
                .any(|unit| unit.seconds.is_some_and(|seconds| seconds > 0.0)),
            "binaries that finished before the kill keep their durations"
        );
    }

    #[test]
    fn the_unit_that_was_running_when_the_child_died_is_named_with_no_duration() {
        let durations = build_test_durations(
            parse_duration_samples(RECORDED_TRUNCATED_OUTPUT),
            Some(BUDGET_SECONDS),
            policy(Some(BUDGET_SECONDS)),
            Some("test child terminated at its budget".to_string()),
        );

        let unfinished = finding(&durations, slow_rule::UNFINISHED_UNIT);
        assert_eq!(
            unfinished.len(),
            1,
            "exactly one binary was mid-flight: {:#?}",
            durations.slow
        );
        let found = unfinished[0];
        assert!(
            found
                .name
                .contains("cook_rejects_local_detach_before_worktree_resolution"),
            "the notice names the test that was running: {}",
            found.name
        );
        assert_eq!(
            found.seconds, None,
            "an unfinished unit has no duration — never zero"
        );
        assert_eq!(
            found.min_seconds,
            Some(60.0),
            "libtest's long-running notice gives a lower bound only"
        );
        assert!(found.incomplete);
        assert_eq!(
            found.budget_share, None,
            "no duration means no share, not a share of zero"
        );
    }

    #[test]
    fn slow_and_unfinished_are_separate_rules() {
        let durations = build_test_durations(
            parse_duration_samples(RECORDED_TRUNCATED_OUTPUT),
            Some(BUDGET_SECONDS),
            policy(Some(BUDGET_SECONDS)),
            Some("terminated".to_string()),
        );
        let rules: std::collections::BTreeSet<&str> = durations
            .slow
            .iter()
            .map(|found| found.rule.as_str())
            .collect();
        assert!(
            rules.contains(slow_rule::UNFINISHED_UNIT),
            "rules were {rules:?}"
        );
        assert!(
            !rules.contains(slow_rule::SLOW_UNIT),
            "a unit with no duration must never be reported as a measured slow test"
        );
    }

    #[test]
    fn a_suite_approaching_its_budget_is_its_own_finding() {
        let durations = build_test_durations(
            parse_duration_samples(RECORDED_CARGO_OUTPUT),
            Some(700.0),
            policy(Some(900.0)),
            None,
        );
        let suite = finding(&durations, slow_rule::SUITE_NEAR_BUDGET);
        assert_eq!(suite.len(), 1, "{:#?}", durations.slow);
        assert!(
            suite[0].message.contains("900s budget"),
            "{}",
            suite[0].message
        );

        // The suite finding does not replace the per-unit finding, and the
        // per-unit finding does not replace it. Different facts, different
        // fixes, both reported.
        assert_eq!(finding(&durations, slow_rule::SLOW_UNIT).len(), 1);
    }

    #[test]
    fn a_fast_suite_under_a_generous_budget_reports_nothing() {
        let durations = build_test_durations(
            parse_duration_samples(
                "     Running tests/quick.rs (/t/deps/quick-0123456789abcdef)\n\
                 test only_test ... ok\n\
                 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.00s\n",
            ),
            Some(12.0),
            policy(Some(1500.0)),
            None,
        );
        // 12 s is 100% of the measured suite but 0.8% of the budget, and below
        // the absolute floor. Flagging it would be noise.
        assert!(durations.slow.is_empty(), "{:#?}", durations.slow);
        assert_eq!(durations.measured_seconds, Some(12.0));
    }

    #[test]
    fn prose_that_merely_starts_with_running_is_not_a_binary() {
        let samples = parse_duration_samples("Running Rust tests...\nRunning cargo test...\n");
        assert!(samples.is_empty(), "{samples:#?}");
    }

    #[test]
    fn per_test_report_time_supersedes_its_binary() {
        let samples = parse_duration_samples(
            "     Running tests/pair.rs (/t/deps/pair-0123456789abcdef)\n\
             test fast_case ... ok <1.000s>\n\
             test slow_case ... ok <299.000s>\n\
             test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 300.00s\n",
        );
        assert_eq!(samples.tests.len(), 2);

        let durations = build_test_durations(samples, None, policy(Some(1000.0)), None);
        let slow = finding(&durations, slow_rule::SLOW_UNIT);
        assert_eq!(slow.len(), 1, "{:#?}", durations.slow);
        assert_eq!(
            slow[0].name, "slow_case",
            "per-test timings localise the cost past the binary"
        );
        assert_eq!(slow[0].seconds, Some(299.0));
    }

    #[test]
    fn nextest_case_and_summary_lines_are_understood() {
        let samples = parse_duration_samples(
            "        PASS [   0.019s] homeboy-core config::tests::loads\n\
                     SLOW [  61.000s] homeboy-core slow::tests::crawls\n\
             ------------\n\
                  Summary [  61.500s] 2 tests run: 2 passed, 0 skipped\n",
        );
        assert_eq!(samples.suite_seconds, Some(61.5));
        assert_eq!(samples.tests.len(), 2);
        assert_eq!(samples.tests[1].seconds, Some(61.0));
        assert_eq!(samples.tests[1].binary.as_deref(), Some("homeboy-core"));

        let durations = build_test_durations(samples, None, policy(Some(300.0)), None);
        assert_eq!(durations.measured_seconds, Some(61.5));
        let slow = finding(&durations, slow_rule::SLOW_UNIT);
        assert_eq!(slow.len(), 1, "{:#?}", durations.slow);
        assert_eq!(slow[0].name, "slow::tests::crawls");
    }

    #[test]
    fn phpunit_time_lines_are_understood_in_both_shapes() {
        assert_eq!(
            parse_phpunit_time_line("Time: 00:01.234, Memory: 6.00 MB"),
            Some(1.234)
        );
        assert_eq!(
            parse_phpunit_time_line("Time: 00:02:03.000, Memory: 6.00 MB"),
            Some(123.0)
        );
        assert_eq!(parse_phpunit_time_line("Time: 1.23 seconds"), Some(1.23));
        assert_eq!(parse_phpunit_time_line("Timely: nope"), None);
    }

    #[test]
    fn identically_named_targets_in_different_crates_stay_distinct() {
        let samples = parse_duration_samples(
            "     Running unittests src/lib.rs (/t/deps/alpha-0123456789abcdef)\n\
             test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s\n\
                  Running unittests src/lib.rs (/t/deps/beta-fedcba9876543210)\n\
             test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.00s\n",
        );
        let names: Vec<&str> = samples
            .binaries
            .iter()
            .map(|unit| unit.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "unittests src/lib.rs (alpha)",
                "unittests src/lib.rs (beta)"
            ],
            "two crates' lib tests must not collapse into one unit"
        );
    }

    #[test]
    fn a_malformed_durations_sidecar_is_ignored_rather_than_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-durations.json");
        std::fs::write(&path, "{ this is not json").expect("write");
        assert_eq!(
            parse_test_durations_file(&path).expect("a bad sidecar is never an error"),
            None
        );
    }

    #[test]
    fn an_absent_durations_sidecar_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            parse_test_durations_file(&dir.path().join("missing.json")).expect("absent is fine"),
            None
        );
    }

    #[test]
    fn a_valid_durations_sidecar_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-durations.json");
        std::fs::write(
            &path,
            r#"{"measured_seconds":12.5,"binaries":[{"name":"tests/a.rs","seconds":12.5,"source":"binary-summary"}]}"#,
        )
        .expect("write");
        let parsed = parse_test_durations_file(&path)
            .expect("valid sidecar parses")
            .expect("sidecar present");
        assert_eq!(parsed.measured_seconds, Some(12.5));
        assert_eq!(parsed.binaries.len(), 1);
        assert!(
            parsed.complete,
            "completeness defaults to true when omitted"
        );
    }
}
