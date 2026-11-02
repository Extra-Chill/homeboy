//! Host pressure has one sampling owner. PSI percentages use the sustained
//! avg300 window; without CPU PSI, load per logical CPU is expressed as percent.
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
pub struct RunnerPressure {
    /// Hundredths of one percent. None means unavailable, not zero pressure.
    pub cpu: Option<u32>,
    pub io: Option<u32>,
    pub memory: Option<u32>,
    pub load_fallback: bool,
    pub admission_reason: Option<String>,
}

impl RunnerPressure {
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
    let line = text.lines().find(|line| line.starts_with(kind))?;
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg300="))?;
    Some((value.parse::<f64>().ok()? * 100.0) as u32)
}

pub fn sample(thresholds: &PressureThresholds) -> RunnerPressure {
    let mut pressure = RunnerPressure {
        cpu: psi("cpu", "some "),
        io: psi("io", "full "),
        memory: psi("memory", "full "),
        ..Default::default()
    };
    if pressure.cpu.is_none() {
        pressure.load_fallback = true;
        pressure.cpu = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
            .zip(std::thread::available_parallelism().ok())
            .map(|(load, cpus)| (load / cpus.get() as f64 * 10_000.0) as u32);
    }
    pressure.apply_thresholds(thresholds);
    pressure
}

#[cfg(test)]
mod tests {
    use super::*;

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
