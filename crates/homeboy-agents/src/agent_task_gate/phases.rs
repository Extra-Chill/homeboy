//! Gate phase spans (#17893).
//!
//! A gate command may print `homeboy-gate-phase name=<id> seconds=<n>` lines
//! on stdout. Each well-formed line becomes one `{name, elapsed_ms}` entry on
//! the gate report, in the order printed. Malformed lines are ordinary output.

use serde::{Deserialize, Serialize};

/// Line prefix a gate prints to report one of its phases.
pub const GATE_PHASE_MARKER: &str = "homeboy-gate-phase";
/// Most phases one gate report records.
pub const MAX_GATE_PHASES: usize = 64;
const MAX_PHASE_NAME_LEN: usize = 64;
/// Ordinary marker lines stay small; only the exact artifact prefix opts in
/// to the separate bounded payload budget.
const MAX_MARKER_LINE_BYTES: usize = 256;
const ARTIFACT_MARKER_PREFIX: &str = "homeboy-gate-test-artifact ";
const MAX_ARTIFACT_LINE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateTestArtifact {
    pub reference: String,
    pub contents: String,
}

impl GateTestArtifact {
    /// Extension-neutral diagnosis contract: JSON artifacts may supply short
    /// top-level cause/message/detail/reason/summary/remediation strings.
    /// Never scrape log text.
    pub(crate) fn diagnosis_summary(&self) -> Vec<String> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&self.contents) else {
            return Vec::new();
        };
        ["cause", "message", "detail", "reason", "summary", "remediation"]
            .into_iter()
            .filter_map(|field| {
                let text = value.get(field)?.as_str()?;
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.is_empty() {
                    return None;
                }
                Some(format!(
                    "{field}: {}",
                    text.chars().take(1024).collect::<String>()
                ))
            })
            .collect()
    }
}

/// One phase a gate reported about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGatePhase {
    pub name: String,
    pub elapsed_ms: u64,
}

/// Parse one stdout line. `None` unless the whole line is a well-formed marker.
pub fn parse_gate_phase_line(line: &str) -> Option<AgentTaskGatePhase> {
    let mut tokens = line.split_whitespace();
    if tokens.next()? != GATE_PHASE_MARKER {
        return None;
    }
    let name = tokens.next()?.strip_prefix("name=")?;
    let seconds = tokens.next()?.strip_prefix("seconds=")?;
    if tokens.next().is_some() || !valid_phase_name(name) {
        return None;
    }
    Some(AgentTaskGatePhase {
        name: name.to_string(),
        elapsed_ms: seconds_to_ms(seconds)?,
    })
}

fn valid_phase_name(name: &str) -> bool {
    (1..=MAX_PHASE_NAME_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// `<digits>[.<digits>]` only: no sign, exponent, `inf` or `nan`.
fn seconds_to_ms(seconds: &str) -> Option<u64> {
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(whole) || !digits(fraction) {
        return None;
    }
    let value: f64 = seconds.parse().ok()?;
    Some((value * 1000.0).round().min(u64::MAX as f64) as u64)
}

/// All phases in `stdout`, in order, capped at [`MAX_GATE_PHASES`].
pub fn gate_phases(stdout: &str) -> Vec<AgentTaskGatePhase> {
    stdout
        .lines()
        .filter_map(parse_gate_phase_line)
        .take(MAX_GATE_PHASES)
        .collect()
}

/// Incremental [`gate_phases`] over a byte stream, so markers survive output
/// the bounded capture later drops from its tail.
#[derive(Debug, Default)]
pub(crate) struct GatePhaseCollector {
    pending: Vec<u8>,
    discarding: bool,
    phases: Vec<AgentTaskGatePhase>,
    pub(crate) artifacts: Vec<GateTestArtifact>,
}

impl GatePhaseCollector {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                if !self.discarding {
                    self.take_line();
                }
                self.pending.clear();
                self.discarding = false;
            } else if !self.discarding {
                let limit = if self.pending.starts_with(ARTIFACT_MARKER_PREFIX.as_bytes()) {
                    MAX_ARTIFACT_LINE_BYTES
                } else {
                    MAX_MARKER_LINE_BYTES
                };
                if self.pending.len() < limit {
                    self.pending.push(byte);
                } else {
                    self.pending.clear();
                    self.discarding = true;
                }
            }
        }
    }

    fn take_line(&mut self) {
        if let Ok(line) = std::str::from_utf8(&self.pending) {
            if let Some(json) = line.strip_prefix(ARTIFACT_MARKER_PREFIX) {
                if self.artifacts.len() < 32
                    && self
                        .artifacts
                        .iter()
                        .map(|a| a.contents.len())
                        .sum::<usize>()
                        + json.len()
                        <= 8 * 1024 * 1024
                {
                    if let Ok(artifact) = serde_json::from_str::<GateTestArtifact>(json) {
                        self.artifacts.push(artifact);
                    }
                }
            }
        }
        if self.phases.len() >= MAX_GATE_PHASES {
            return;
        }
        if let Some(phase) = std::str::from_utf8(&self.pending)
            .ok()
            .and_then(parse_gate_phase_line)
        {
            self.phases.push(phase);
        }
    }

    pub(crate) fn finish(mut self) -> Vec<AgentTaskGatePhase> {
        if !self.discarding {
            self.take_line();
        }
        self.phases
    }
}

/// The slowest phase of `phases`; the first one printed wins a tie.
pub fn slowest_gate_phase<'a>(
    phases: impl IntoIterator<Item = &'a AgentTaskGatePhase>,
) -> Option<&'a AgentTaskGatePhase> {
    phases.into_iter().fold(
        None,
        |slowest: Option<&AgentTaskGatePhase>, phase| match slowest {
            Some(current) if current.elapsed_ms >= phase.elapsed_ms => Some(current),
            _ => Some(phase),
        },
    )
}

/// One gate's recorded phases, read from a persisted gate report.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RecordedGatePhases {
    pub gate: String,
    pub phases: Vec<AgentTaskGatePhase>,
}

/// The phases of each gate in a persisted promotion report that recorded any.
pub fn recorded_gate_phases(promotion: &serde_json::Value) -> Vec<RecordedGatePhases> {
    promotion
        .get("deterministic_gates")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|gate| {
            let phases: Vec<AgentTaskGatePhase> =
                serde_json::from_value(gate.get("phases")?.clone()).ok()?;
            (!phases.is_empty()).then(|| RecordedGatePhases {
                gate: gate
                    .get("label")
                    .or_else(|| gate.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("gate")
                    .to_string(),
                phases,
            })
        })
        .collect()
}

/// `acceptance 3.6m` for the slowest phase across `gates`.
pub fn slowest_recorded_gate_phase_text(gates: &[RecordedGatePhases]) -> Option<String> {
    slowest_gate_phase(gates.iter().flat_map(|gate| &gate.phases))
        .map(|phase| format!("{} {}", phase.name, format_minutes(phase.elapsed_ms)))
}

/// Minutes with one decimal, as Cook summaries print durations (`3.6m`).
pub fn format_minutes(ms: u64) -> String {
    format!("{:.1}m", ms as f64 / 60_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnosis_summary_preserves_field_order_and_character_cap() {
        let fields = ["cause", "message", "detail", "reason", "summary", "remediation"];
        let contents: serde_json::Value = fields
            .iter()
            .map(|field| ((*field).to_string(), serde_json::json!("é".repeat(1025))))
            .collect::<serde_json::Map<_, _>>()
            .into();
        let artifact = GateTestArtifact {
            reference: "diagnosis".to_string(),
            contents: contents.to_string(),
        };
        let expected: Vec<_> = fields
            .iter()
            .map(|field| format!("{field}: {}", "é".repeat(1024)))
            .collect();
        assert_eq!(artifact.diagnosis_summary(), expected);
    }

    #[test]
    fn gate_phase_lines_parse_only_the_documented_shape() {
        let phase = |name: &str, elapsed_ms| {
            Some(AgentTaskGatePhase {
                name: name.to_string(),
                elapsed_ms,
            })
        };
        assert_eq!(
            parse_gate_phase_line("homeboy-gate-phase name=tests seconds=109"),
            phase("tests", 109_000)
        );
        assert_eq!(
            parse_gate_phase_line("homeboy-gate-phase name=a.b_c-1 seconds=0.25\r"),
            phase("a.b_c-1", 250)
        );
        for malformed in [
            "homeboy-gate-phase name=tests",
            "homeboy-gate-phase name= seconds=1",
            "homeboy-gate-phase name=bad/name seconds=1",
            "homeboy-gate-phase name=tests seconds=-1",
            "homeboy-gate-phase name=tests seconds=1e3",
            "homeboy-gate-phase name=tests seconds=inf",
            "homeboy-gate-phase name=tests seconds=1.",
            "homeboy-gate-phase name=tests seconds=1 extra",
            "homeboy-gate-phase seconds=1 name=tests",
            "prefix homeboy-gate-phase name=tests seconds=1",
            &format!("homeboy-gate-phase name={} seconds=1", "x".repeat(65)),
        ] {
            assert_eq!(parse_gate_phase_line(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn oversized_non_artifact_marker_stays_capped_at_256_bytes() {
        let mut collector = GatePhaseCollector::default();
        collector.push(b"homeboy-gate-phase name=tests seconds=");
        collector.push(&vec![b'1'; 256 - collector.pending.len()]);
        assert_eq!(collector.pending.len(), 256);
        collector.push(b"1");
        assert!(collector.discarding);
        assert!(collector.pending.is_empty());
        collector.push(&vec![b'x'; 1024 * 1024]);
        assert!(collector.pending.is_empty());
        collector.push(b"\nhomeboy-gate-phase name=recovered seconds=1\n");
        assert_eq!(collector.finish()[0].name, "recovered");
    }

    #[test]
    fn artifact_marker_has_a_separate_bounded_budget() {
        let artifact = GateTestArtifact {
            reference: "homeboy://run/test/artifact/large".into(),
            contents: "x".repeat(1024),
        };
        let line = format!(
            "{ARTIFACT_MARKER_PREFIX}{}\n",
            serde_json::to_string(&artifact).unwrap()
        );
        let mut collector = GatePhaseCollector::default();
        for chunk in line.as_bytes().chunks(7) {
            collector.push(chunk);
        }
        assert_eq!(collector.artifacts, vec![artifact]);
        collector.push(ARTIFACT_MARKER_PREFIX.as_bytes());
        collector.push(&vec![b'x'; MAX_ARTIFACT_LINE_BYTES]);
        assert!(collector.discarding);
        assert!(collector.pending.is_empty());
    }

    #[test]
    fn gate_phase_collector_matches_the_string_parser_and_caps() {
        let stdout = (0..70)
            .map(|index| format!("noise\nhomeboy-gate-phase name=p{index} seconds={index}\n"))
            .collect::<String>();
        let mut collector = GatePhaseCollector::default();
        for chunk in stdout.as_bytes().chunks(7) {
            collector.push(chunk);
        }
        let streamed = collector.finish();
        assert_eq!(streamed, gate_phases(&stdout));
        assert_eq!(streamed.len(), MAX_GATE_PHASES);
        assert_eq!(streamed[3].name, "p3");
        assert_eq!(slowest_gate_phase(&streamed).unwrap().name, "p63");
    }
}
