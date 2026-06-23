use super::super::*;
use types::RunnerDoctorStatus;

#[test]
fn headed_browser_check_warns_without_display_or_xvfb() {
    let check = checks::headed_browser_check(false, false, false);

    assert_eq!(check.id, "browser.headed_ready");
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert_eq!(
        check.details.get("display_ready").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        check.details.get("xvfb_ready").map(String::as_str),
        Some("false")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("headless/Ozone")));
}

#[test]
fn headed_browser_ready_accepts_display_or_xvfb() {
    assert!(probes::headed_browser_ready(true, false));
    assert!(probes::headed_browser_ready(false, true));
    assert!(!probes::headed_browser_ready(false, false));
}
