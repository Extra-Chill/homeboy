//! Host pressure has one sampling owner. PSI percentages use the avg60 window
//! for timely admission recovery; without CPU PSI, load per logical CPU is percent.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PressureThresholds {
    pub cpu: u32,
    pub io: u32,
    pub memory: u32,
    #[serde(default = "default_load_threshold")]
    pub load_per_cpu: u32,
}

fn default_load_threshold() -> u32 {
    100
}

impl Default for PressureThresholds {
    fn default() -> Self {
        Self {
            cpu: 10,
            io: 10,
            memory: 5,
            load_per_cpu: default_load_threshold(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunnerPressure {
    /// Hundredths of one percent. None means unavailable, not zero pressure.
    pub cpu: Option<u32>,
    pub io: Option<u32>,
    pub memory: Option<u32>,
    pub load_fallback: bool,
    /// Five-minute load per logical CPU, in ten-thousandths (1.0 = 10000).
    pub observed_load_per_cpu: Option<u32>,
    pub admission_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation: Option<serde_json::Value>,
}

impl RunnerPressure {
    /// A pre-acceptance wait, using the execution scheduler's capacity deferral
    /// contract rather than consuming its infrastructure recovery budget.
    pub fn admission_refusal(&self) -> crate::Error {
        let mut error = crate::Error::validation_invalid_argument(
            "admission",
            "admission: pressure — waiting for host pressure to clear",
            None,
            None,
        )
        .with_retryable(true);
        error.details["reason"] = serde_json::json!("runner_pressure");
        error.details["admission_reason"] = serde_json::json!("pressure");
        error.details["pressure"] = serde_json::json!(self);
        error.details["execution_step_disposition"] = serde_json::json!("runner_ineligible");
        error
    }

    pub fn apply_thresholds(&mut self, thresholds: &PressureThresholds) {
        self.admission_reason = self.above(thresholds).then(|| "pressure".to_string());
    }
    pub fn above(&self, thresholds: &PressureThresholds) -> bool {
        [
            (
                self.cpu,
                if self.load_fallback {
                    thresholds.load_per_cpu
                } else {
                    thresholds.cpu
                },
            ),
            (self.io, thresholds.io),
            (self.memory, thresholds.memory),
        ]
        .iter()
        .any(|(value, limit)| value.is_some_and(|value| value > limit.saturating_mul(100)))
    }
}

fn psi(resource: &str, kind: &str) -> Option<u32> {
    let text = std::fs::read_to_string(format!("/proc/pressure/{resource}")).ok()?;
    parse_psi(&text, kind)
}

fn parse_psi(text: &str, kind: &str) -> Option<u32> {
    let line = text.lines().find(|line| line.starts_with(kind))?;
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg60="))?;
    Some((value.parse::<f64>().ok()? * 100.0) as u32)
}

/// Optional fixed JSON sample for controlled daemon environments. Unset or
/// malformed values retain live host sampling; thresholds are always reapplied.
pub const SAMPLE_ENV: &str = "HOMEBOY_RUNNER_PRESSURE_SAMPLE";

fn configured_sample(value: Option<&str>) -> Option<RunnerPressure> {
    serde_json::from_str(value?).ok()
}

pub fn sample(thresholds: &PressureThresholds) -> RunnerPressure {
    if let Some(mut pressure) = configured_sample(std::env::var(SAMPLE_ENV).ok().as_deref()) {
        pressure.apply_thresholds(thresholds);
        return pressure;
    }
    sample_host(thresholds)
}

fn sample_host(thresholds: &PressureThresholds) -> RunnerPressure {
    let mut pressure = RunnerPressure {
        cpu: psi("cpu", "some "),
        io: psi("io", "full "),
        memory: psi("memory", "full "),
        ..Default::default()
    };
    pressure.observed_load_per_cpu = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().nth(1)?.parse::<f64>().ok())
        .zip(std::thread::available_parallelism().ok())
        .map(|(load, cpus)| (load / cpus.get() as f64 * 10_000.0) as u32);
    if pressure.cpu.is_none() {
        pressure.load_fallback = true;
        pressure.cpu = pressure.observed_load_per_cpu;
    }
    pressure.apply_thresholds(thresholds);
    pressure
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_admission_recovers_despite_high_five_minute_pressure() {
        let text = "some avg10=0.00 avg60=0.65 avg300=24.14 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        let mut pressure = RunnerPressure {
            cpu: parse_psi(text, "some "),
            ..Default::default()
        };
        assert_eq!(pressure.cpu, Some(65));
        pressure.apply_thresholds(&PressureThresholds::default());
        assert!(pressure.admission_reason.is_none());
    }

    #[test]
    fn psi_admission_refuses_high_one_minute_pressure() {
        let text = "some avg10=0.00 avg60=12.50 avg300=4.14 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        let mut pressure = RunnerPressure {
            cpu: parse_psi(text, "some "),
            ..Default::default()
        };
        assert_eq!(pressure.cpu, Some(1250));
        pressure.apply_thresholds(&PressureThresholds::default());
        assert_eq!(pressure.admission_reason.as_deref(), Some("pressure"));
        pressure.apply_thresholds(&PressureThresholds {
            cpu: 20,
            ..Default::default()
        });
        assert!(pressure.admission_reason.is_none());
    }

    #[test]
    fn psi_full_uses_the_same_window_and_unavailable_is_not_zero() {
        let text = "some avg10=30.00 avg60=20.00 avg300=10.00 total=123\nfull avg10=9.00 avg60=6.25 avg300=2.00 total=100\n";
        assert_eq!(parse_psi(text, "full "), Some(625));
        assert_eq!(parse_psi("some avg300=20.00", "some "), None);
        assert_eq!(parse_psi("some avg60=invalid", "some "), None);
        assert_eq!(parse_psi(text, "missing "), None);
    }

    #[test]
    fn pressure_sample_source_overrides_host_for_admission_and_telemetry() {
        let _lock = crate::test_support::env_lock();
        let _source = crate::test_support::EnvVarGuard::set(SAMPLE_ENV, r#"{"cpu":2185}"#);
        let pressure = sample(&PressureThresholds::default());
        assert_eq!(pressure.cpu, Some(2185));
        assert_eq!(pressure.admission_reason.as_deref(), Some("pressure"));
        let _quiet = crate::test_support::EnvVarGuard::set(SAMPLE_ENV, "{}");
        assert_eq!(
            sample(&PressureThresholds::default()),
            RunnerPressure::default()
        );
    }

    #[test]
    fn runtime_snapshot_displays_the_limits_used_for_admission() {
        // Older daemon evidence without limits still deserializes.
        let mut snapshot: crate::daemon::DaemonRuntimeSnapshot = serde_json::from_value(
            serde_json::json!({"pressure": {"cpu": 1250}, "loaded_at": "now", "paths": []}),
        )
        .unwrap();
        assert_eq!(snapshot.pressure_thresholds, PressureThresholds::default());
        snapshot.apply_pressure_thresholds(PressureThresholds::default());
        assert!(snapshot
            .pressure
            .as_ref()
            .unwrap()
            .admission_reason
            .is_some());
        snapshot.apply_pressure_thresholds(PressureThresholds {
            cpu: 20,
            ..Default::default()
        });
        let output = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(output["pressure_thresholds"]["cpu"], 20);
        assert_eq!(output["pressure"]["cpu"], 1250);
        assert!(output["pressure"]["admission_reason"].is_null());
    }

    #[test]
    fn fixed_pressure_sample_reapplies_thresholds() {
        let mut pressure = configured_sample(Some(r#"{"cpu":2185}"#)).unwrap();
        pressure.apply_thresholds(&PressureThresholds::default());
        assert_eq!(pressure.admission_reason.as_deref(), Some("pressure"));
        pressure.apply_thresholds(&PressureThresholds {
            cpu: 30,
            ..Default::default()
        });
        assert!(pressure.admission_reason.is_none());
        let mut quiet = configured_sample(Some(r#"{}"#)).unwrap();
        quiet.apply_thresholds(&PressureThresholds::default());
        assert!(quiet.admission_reason.is_none());
        assert!(configured_sample(None).is_none());
        assert!(configured_sample(Some("invalid")).is_none());
    }

    #[test]
    fn admission_refused_above_threshold() {
        let mut pressure = RunnerPressure {
            io: Some(1001),
            ..Default::default()
        };
        pressure.apply_thresholds(&PressureThresholds::default());
        assert_eq!(pressure.admission_reason.as_deref(), Some("pressure"));
    }

    #[test]
    fn controller_thresholds_can_clear_a_default_pressure_verdict() {
        let mut pressure = RunnerPressure {
            cpu: Some(1100),
            ..Default::default()
        };
        pressure.apply_thresholds(&PressureThresholds::default());
        assert!(pressure.admission_reason.is_some());
        pressure.apply_thresholds(&PressureThresholds {
            cpu: 20,
            ..Default::default()
        });
        assert!(pressure.admission_reason.is_none());
    }

    #[test]
    fn load_fallback_uses_its_own_limit() {
        let mut pressure = RunnerPressure {
            cpu: Some(10_000),
            load_fallback: true,
            ..Default::default()
        };
        pressure.apply_thresholds(&PressureThresholds::default());
        assert!(pressure.admission_reason.is_none());
        pressure.cpu = Some(10_001);
        pressure.apply_thresholds(&PressureThresholds::default());
        assert!(pressure.admission_reason.is_some());
    }

    #[test]
    fn admission_allowed_below_threshold() {
        let mut pressure = RunnerPressure {
            cpu: Some(900),
            io: Some(900),
            memory: Some(400),
            ..Default::default()
        };
        pressure.apply_thresholds(&PressureThresholds::default());
        assert!(pressure.admission_reason.is_none());
    }
}
