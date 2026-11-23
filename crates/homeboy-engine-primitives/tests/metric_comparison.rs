use homeboy_engine_primitives::metric_comparison::{
    Boundary, Direction, MetricChange, MetricPolicy, RegressionTest,
};

#[test]
fn threshold_boundaries_and_improvements_follow_instrument_policy() {
    let mut policy = MetricPolicy {
        direction: Some(Direction::HigherIsBetter),
        percent: Some(10.0),
        ..Default::default()
    };
    assert_eq!(
        policy.compare("throughput", Some(100.0), Some(90.0)).change,
        MetricChange::Unchanged
    );
    policy.boundary = Boundary::Inclusive;
    assert!(policy
        .compare("throughput", Some(100.0), Some(90.0))
        .regressed());
    policy.boundary = Boundary::Strict;
    assert!(policy
        .compare("throughput", Some(100.0), Some(101.0))
        .improved());
    policy.threshold_improvements = true;
    assert!(!policy
        .compare("throughput", Some(100.0), Some(101.0))
        .improved());
}

#[test]
fn zero_baseline_missing_and_distribution_evidence_remain_distinct() {
    let mut policy = MetricPolicy {
        direction: Some(Direction::LowerIsBetter),
        percent: Some(5.0),
        ..Default::default()
    };
    let delta = policy.compare("latency", Some(0.0), Some(10.0));
    assert!(delta.regressed());
    assert_eq!(delta.percent, None);
    policy.zero_baseline_is_neutral = true;
    assert!(!policy.compare("latency", Some(0.0), Some(10.0)).regressed());
    assert_eq!(
        policy.compare("latency", Some(10.0), None).change,
        MetricChange::Missing
    );
    assert!(policy
        .compare_distribution("latency", &[], &[10.0], RegressionTest::PointDelta)
        .is_none());
    let delta = policy
        .compare_distribution(
            "latency",
            &[100.0, 110.0, 120.0],
            &[120.0, 130.0, 140.0],
            RegressionTest::PointDelta,
        )
        .unwrap();
    assert!(delta.regressed());
    assert_eq!(delta.baseline, Some(110.0));
    assert_eq!(delta.current_samples, Some(3));
}
