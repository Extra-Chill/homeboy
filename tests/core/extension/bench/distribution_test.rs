use super::{distribution, percentile};

#[test]
fn distribution_reports_population_summary() {
    let summary = distribution(&[1.0, 2.0, 3.0]);

    assert_eq!(summary.n, 3);
    assert_eq!(summary.min, 1.0);
    assert_eq!(summary.max, 3.0);
    assert_eq!(summary.mean, 2.0);
    assert!((summary.stdev - (2.0_f64 / 3.0).sqrt()).abs() < 1e-9);
    assert!((summary.cv_pct - summary.stdev / 2.0 * 100.0).abs() < 1e-9);
    assert_eq!(summary.p50, 2.0);
    assert_eq!(summary.p95, 2.9);
}

#[test]
fn distribution_handles_zero_mean_cv() {
    let summary = distribution(&[0.0, 0.0, 0.0]);

    assert_eq!(summary.cv_pct, 0.0);
    assert!(summary.cv_pct.is_finite());
}

#[test]
fn percentile_interpolates_r7_style() {
    assert_eq!(percentile(&[100.0, 200.0, 300.0], 50.0), 200.0);
    assert_eq!(percentile(&[100.0, 200.0, 300.0], 95.0), 290.0);
    assert_eq!(percentile(&[42.0], 95.0), 42.0);
}
