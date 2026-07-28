//! Turn a test runner's captured output into duration samples.
//!
//! Split from the policy module because the two answer different questions:
//! this file only asks "what did the runner say took how long", and knows
//! nothing about budgets, thresholds, or findings.

use homeboy_extension_contract::test_duration::{duration_source, output_marker, TestUnitDuration};

use super::TestDurationSamples;

/// Prefix of a per-case outcome line in the line-oriented runner format.
const CASE_LINE_PREFIX: &str = "test ";

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

        if let Some(seconds) = parse_suite_summary_line(trimmed) {
            samples.suite_seconds = Some(seconds);
            continue;
        }

        if let Some(seconds) = parse_suite_elapsed_line(trimmed) {
            samples.suite_seconds = Some(seconds);
            continue;
        }

        if let Some(unit) = parse_bracketed_case_line(trimmed) {
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
    if let Some(rest) = trimmed.strip_prefix(output_marker::TARGET_START) {
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
    if let Some(crate_name) = trimmed.strip_prefix(output_marker::DOC_TARGET_START) {
        let crate_name = crate_name.trim();
        if crate_name.is_empty() || crate_name.contains(' ') {
            return None;
        }
        return Some(TestUnitDuration::new(
            format!("{}{crate_name}", output_marker::DOC_TARGET_START),
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
    if !trimmed.starts_with(output_marker::BINARY_SUMMARY) {
        return None;
    }
    let seconds = after_marker(trimmed, output_marker::ELAPSED)
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
    let rest = line.trim_start().strip_prefix(CASE_LINE_PREFIX)?;
    let marker = rest.find(output_marker::CASE_OUTCOME)?;
    let name = rest[..marker].trim();
    if name.is_empty() {
        return None;
    }
    let outcome = rest[marker + output_marker::CASE_OUTCOME.len()..].trim();
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
    let rest = line.trim_start().strip_prefix(CASE_LINE_PREFIX)?;
    let marker = rest.find(output_marker::LONG_RUNNING)?;
    let name = rest[..marker].trim();
    let tail = rest[marker + output_marker::LONG_RUNNING.len()..].trim();
    let seconds = tail
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    (!name.is_empty()).then(|| (name.to_string(), seconds))
}

/// A per-case line whose duration is bracketed after an outcome keyword —
/// `    PASS [   0.019s] homeboy-core config::tests::name`.
fn parse_bracketed_case_line(line: &str) -> Option<TestUnitDuration> {
    let trimmed = line.trim_start();
    let status_end = trimmed.find(" [")?;
    let status = trimmed[..status_end].trim();
    if !output_marker::CASE_OUTCOMES.contains(&status) {
        return None;
    }
    let rest = &trimmed[status_end + 2..];
    let close = rest.find(']')?;
    let seconds: f64 = rest[..close].trim().trim_end_matches('s').parse().ok()?;
    let mut parts = rest[close + 1..].split_whitespace();
    let binary = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    let mut unit = TestUnitDuration::new(name, duration_source::RUNNER_CASE);
    unit.binary = Some(binary);
    unit.seconds = Some(seconds);
    Some(unit)
}

/// A whole-suite aggregate with its elapsed time —
/// `     Summary [  12.345s] 100 tests run: 100 passed`.
fn parse_suite_summary_line(line: &str) -> Option<f64> {
    let rest = line
        .trim_start()
        .strip_prefix(output_marker::SUITE_SUMMARY)?;
    let close = rest.find(']')?;
    rest[..close].trim().trim_end_matches('s').parse().ok()
}

/// A whole-suite elapsed line — `Time: 00:01.234, Memory: 6.00 MB` or
/// `Time: 1.23 seconds`.
pub(super) fn parse_suite_elapsed_line(line: &str) -> Option<f64> {
    let rest = line
        .trim_start()
        .strip_prefix(output_marker::SUITE_ELAPSED)?;
    let value = rest.split(',').next()?.trim();
    if let Some(seconds) = value
        .strip_suffix(" seconds")
        .or(value.strip_suffix(" second"))
    {
        return seconds.trim().parse().ok();
    }
    // `00:01.234` (mm:ss) and `00:00:01.234` (hh:mm:ss) are both emitted
    // depending on runner version; fold left so either works.
    let mut total = 0.0;
    for part in value.split(':') {
        let part: f64 = part.trim().parse().ok()?;
        total = total * 60.0 + part;
    }
    Some(total)
}
