//! Operator measurement windows share the daemon's pressure sampling and queue.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub id: String,
    pub state: String,
    /// Five-minute load average divided by logical CPU count (1.0 = full load).
    pub threshold: f64,
    pub last_observed_load: Option<f64>,
    pub active_until: Option<u64>,
    pub reason: Option<String>,
    wait_until: u64,
    window_ms: u64,
}
impl Reservation {
    fn admits(&self, holder: Option<&str>) -> bool {
        !self.held() || (self.state == "active" && holder == Some(self.id.as_str()))
    }
    fn held(&self) -> bool {
        matches!(self.state.as_str(), "waiting_for_quiet" | "active")
    }
    fn advance(&mut self, now: u64, observed: Option<f64>) {
        self.last_observed_load = observed;
        if self.state == "waiting_for_quiet" {
            if now >= self.wait_until {
                self.state = "reservation_wait_timeout".into();
            } else if observed.is_some_and(|load| load < self.threshold) {
                self.state = "active".into();
                self.active_until = Some(now.saturating_add(self.window_ms));
            }
        }
        if self.state == "active" && self.active_until.is_some_and(|end| now >= end) {
            self.state = "expired".into();
        }
    }
}
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn path() -> Result<PathBuf> {
    Ok(state_path()?.with_file_name("runner-reservation.json"))
}
fn load() -> Result<Option<Reservation>> {
    match std::fs::read(path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::internal_unexpected(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::internal_unexpected(e.to_string())),
    }
}
fn save(value: &Reservation) -> Result<()> {
    let path = path()?;
    let temp = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    use std::io::Write;
    let mut file =
        std::fs::File::create(&temp).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| Error::internal_unexpected(e.to_string()))?;
    std::fs::rename(temp, &path).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    std::fs::File::open(path.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::internal_unexpected(e.to_string()))
}
fn observe(value: &mut Reservation) {
    let pressure = crate::runner_pressure::sample(&Default::default());
    value.advance(
        crate::api_jobs::timestamp_ms(),
        pressure.observed_load_per_cpu.map(|n| n as f64 / 10000.0),
    );
}
pub(super) fn current() -> Result<Option<Reservation>> {
    let _lock = LOCK.lock().unwrap();
    let mut value = load()?;
    if let Some(value) = value.as_mut() {
        observe(value);
        save(value)?;
    }
    Ok(value)
}
pub(crate) fn holding(holder: Option<&str>) -> Result<Option<String>> {
    Ok(current()?.filter(|r| !r.admits(holder)).map(|r| r.id))
}
pub(super) fn update(body: Option<serde_json::Value>) -> Result<serde_json::Value> {
    let _lock = LOCK.lock().unwrap();
    let body = body.unwrap_or_default();
    let mut previous = load()?;
    if let Some(value) = previous.as_mut() {
        observe(value);
    }
    let value = if let Some(id) = body.get("release").and_then(|v| v.as_str()) {
        let mut value = previous.filter(|r| r.id == id).ok_or_else(|| {
            Error::validation_invalid_argument("release", "unknown reservation id", None, None)
        })?;
        if value.held() {
            value.state = "released".into();
        }
        value
    } else {
        if previous.as_ref().is_some_and(Reservation::held) {
            return Err(Error::validation_invalid_argument(
                "reservation",
                "runner already reserved",
                None,
                None,
            ));
        }
        let threshold = body
            .get("quiet_below")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "quiet_below",
                    "expected non-negative load per CPU",
                    None,
                    None,
                )
            })?;
        let minutes = |key: &str, default: Option<u64>| -> Result<u64> {
            body.get(key)
                .and_then(|v| v.as_u64())
                .or(default)
                .filter(|n| *n > 0 && *n <= 525600)
                .map(|n| n * 60000)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        "expected minutes between 1 and 525600",
                        None,
                        None,
                    )
                })
        };
        let mut value = Reservation {
            id: Uuid::new_v4().to_string(),
            state: "waiting_for_quiet".into(),
            threshold,
            last_observed_load: None,
            active_until: None,
            reason: body
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            wait_until: crate::api_jobs::timestamp_ms() + minutes("wait_timeout", Some(30))?,
            window_ms: minutes("window", None)?,
        };
        observe(&mut value);
        value
    };
    save(&value)?;
    Ok(json!(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_survives_reload_and_expires_without_holder() {
        crate::test_support::with_isolated_home(|_| {
            super::super::tests::write_test_lease("reservation-restart");
            let created = update(Some(json!({"quiet_below": 0, "window": 1}))).unwrap();
            let mut restored = load().unwrap().unwrap();
            assert_eq!(restored.id, created["id"].as_str().unwrap());
            restored.state = "active".into();
            restored.active_until = Some(0);
            save(&restored).unwrap();
            assert_eq!(current().unwrap().unwrap().state, "expired");
            assert!(holding(None).unwrap().is_none());
        });
    }

    #[test]
    fn reservation_transitions() {
        for (load, now, expected) in [
            (Some(1.0), 1, "active"),
            (Some(9.0), 10, "reservation_wait_timeout"),
        ] {
            let mut r = Reservation {
                id: "test".into(),
                state: "waiting_for_quiet".into(),
                threshold: 2.0,
                last_observed_load: None,
                active_until: None,
                reason: None,
                wait_until: 10,
                window_ms: 20,
            };
            r.advance(now, load);
            assert_eq!(r.state, expected);
            if expected == "active" {
                assert!(r.admits(Some("test")));
                assert!(!r.admits(None));
                assert!(!r.admits(Some("wrong")));
                let mut released = r.clone();
                released.state = "released".into();
                assert!(!released.held());
                r.advance(21, load);
                assert_eq!(r.state, "expired");
                assert!(!r.held());
            }
        }
    }
}
