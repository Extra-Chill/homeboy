//! Opt-in regression delivery through the existing notification outbox.
use super::{RegressionComparison, Verdict};
use crate::notification_route::NotificationRoute;
use sha2::{Digest, Sha256};

/// The analytics query stays read-only. Only explicit notification delivery
/// creates a claim, outside the observation ledger.
pub fn notify_regression(comparison: &RegressionComparison, route: Option<&NotificationRoute>) {
    if comparison.verdict != Verdict::Regressed {
        return;
    }
    let Some((baseline, candidate)) = comparison
        .baseline_version
        .as_ref()
        .zip(comparison.candidate_version.as_ref())
    else {
        return;
    };
    let Ok(root) = crate::paths::homeboy() else {
        return;
    };
    let dir = root.join("hotspot-notifications");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        crate::log_status!(
            "runs",
            "cannot create regression notification directory: {}",
            error
        );
        return;
    }
    // Encode the pair without trusting version strings as filesystem paths.
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(baseline, candidate)).unwrap_or_default())
    );
    let claim = dir.join(key);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&claim)
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return,
        Err(error) => {
            crate::log_status!("runs", "cannot claim regression notification: {}", error);
            return;
        }
    }
    let mut event = crate::notify::NotifyEvent::run_completed_with_route(
        &format!("hotspots:{baseline}:{candidate}"),
        "regressed",
        route,
    )
    .with_title(format!("Homeboy regression: {baseline} → {candidate}"));
    event.body = serde_json::to_string_pretty(comparison).unwrap_or_default();
    let delivery = crate::notify_outbox::dispatch_with_outbox(&event, None);
    if delivery.disposition == crate::notify_outbox::NotifyOutboxDisposition::Dropped {
        let _ = std::fs::remove_file(claim);
    }
}
