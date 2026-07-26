use super::{ProcessRow, ResourceRecommendation};

pub(super) fn classify_load(
    averages: Option<[f64; 3]>,
    cpu_count: usize,
) -> ResourceRecommendation {
    let Some([one, five, _]) = averages else {
        return ResourceRecommendation::Ok;
    };
    let cpus = cpu_count.max(1) as f64;
    let one_ratio = one / cpus;
    let five_ratio = five / cpus;

    if one_ratio >= 1.5 || five_ratio >= 1.25 {
        ResourceRecommendation::Hot
    } else if one_ratio >= 0.75 || five_ratio >= 0.75 {
        ResourceRecommendation::Warm
    } else {
        ResourceRecommendation::Ok
    }
}

pub(super) fn classify_memory(total_bytes: u64, available_bytes: u64) -> ResourceRecommendation {
    if total_bytes == 0 {
        return ResourceRecommendation::Ok;
    }
    let available_ratio = available_bytes as f64 / total_bytes as f64;
    if available_ratio <= 0.10 {
        ResourceRecommendation::Hot
    } else if available_ratio <= 0.20 {
        ResourceRecommendation::Warm
    } else {
        ResourceRecommendation::Ok
    }
}

const PROCESS_WARM_CPU_PER_CORE_PERCENT: f64 = 12.5;
const PROCESS_HOT_CPU_PER_CORE_PERCENT: f64 = 25.0;
const PROCESS_WARM_CPU_FLOOR_PERCENT: f64 = 50.0;
const PROCESS_HOT_CPU_FLOOR_PERCENT: f64 = 100.0;
const PROCESS_WARM_RSS_RATIO: f64 = 0.125;
const PROCESS_HOT_RSS_RATIO: f64 = 0.25;
const PROCESS_WARM_RSS_FLOOR_MB: u64 = 512;
const PROCESS_HOT_RSS_FLOOR_MB: u64 = 1024;

pub(super) fn classify_processes(
    rows: &[ProcessRow],
    cpu_count: usize,
    total_memory_mb: Option<u64>,
) -> ResourceRecommendation {
    let warm_cpu_percent = (cpu_count.max(1) as f64 * PROCESS_WARM_CPU_PER_CORE_PERCENT)
        .max(PROCESS_WARM_CPU_FLOOR_PERCENT);
    let hot_cpu_percent = (cpu_count.max(1) as f64 * PROCESS_HOT_CPU_PER_CORE_PERCENT)
        .max(PROCESS_HOT_CPU_FLOOR_PERCENT);
    let warm_rss_mb = total_memory_mb.map(|total| (total as f64 * PROCESS_WARM_RSS_RATIO) as u64);
    let hot_rss_mb = total_memory_mb.map(|total| (total as f64 * PROCESS_HOT_RSS_RATIO) as u64);

    if rows.iter().any(|row| {
        row.cpu_percent >= hot_cpu_percent
            || hot_rss_mb
                .is_some_and(|threshold| row.rss_mb >= threshold.max(PROCESS_HOT_RSS_FLOOR_MB))
    }) {
        ResourceRecommendation::Hot
    } else if rows.iter().any(|row| {
        row.cpu_percent >= warm_cpu_percent
            || warm_rss_mb
                .is_some_and(|threshold| row.rss_mb >= threshold.max(PROCESS_WARM_RSS_FLOOR_MB))
    }) {
        ResourceRecommendation::Warm
    } else {
        ResourceRecommendation::Ok
    }
}

pub(super) fn classify_rig_leases(
    active_count: usize,
    concurrency_limit: Option<usize>,
) -> ResourceRecommendation {
    let Some(limit) = concurrency_limit.filter(|limit| *limit > 0) else {
        return match active_count {
            0 => ResourceRecommendation::Ok,
            1 => ResourceRecommendation::Warm,
            _ => ResourceRecommendation::Hot,
        };
    };

    if active_count >= limit {
        ResourceRecommendation::Hot
    } else if active_count.saturating_mul(4) >= limit.saturating_mul(3) {
        ResourceRecommendation::Warm
    } else {
        ResourceRecommendation::Ok
    }
}

pub(super) fn overall_recommendation(values: &[ResourceRecommendation]) -> ResourceRecommendation {
    values
        .iter()
        .copied()
        .max()
        .unwrap_or(ResourceRecommendation::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_load_by_cpu_normalized_pressure() {
        assert_eq!(
            classify_load(Some([1.0, 1.0, 1.0]), 4),
            ResourceRecommendation::Ok
        );
        assert_eq!(
            classify_load(Some([3.1, 2.0, 1.0]), 4),
            ResourceRecommendation::Warm
        );
        assert_eq!(
            classify_load(Some([6.0, 4.0, 2.0]), 4),
            ResourceRecommendation::Hot
        );
    }

    #[test]
    fn classifies_memory_by_available_ratio() {
        assert_eq!(classify_memory(100, 30), ResourceRecommendation::Ok);
        assert_eq!(classify_memory(100, 20), ResourceRecommendation::Warm);
        assert_eq!(classify_memory(100, 10), ResourceRecommendation::Hot);
    }

    #[test]
    fn classifies_processes_by_capacity_relative_cpu_and_rss() {
        let rows = vec![ProcessRow {
            pid: 1,
            cpu_percent: 25.0,
            rss_mb: 512,
            command: "homeboy".to_string(),
            args: "homeboy bench".to_string(),
        }];
        assert_eq!(
            classify_processes(&rows, 4, Some(8 * 1024)),
            ResourceRecommendation::Ok
        );

        let rows = vec![ProcessRow {
            cpu_percent: 51.0,
            ..rows[0].clone()
        }];
        assert_eq!(
            classify_processes(&rows, 4, Some(8 * 1024)),
            ResourceRecommendation::Warm
        );

        let rows = vec![ProcessRow {
            cpu_percent: 101.0,
            ..rows[0].clone()
        }];
        assert_eq!(
            classify_processes(&rows, 4, Some(8 * 1024)),
            ResourceRecommendation::Hot
        );

        let rows = vec![ProcessRow {
            rss_mb: 2 * 1024,
            ..rows[0].clone()
        }];
        assert_eq!(
            classify_processes(&rows, 4, Some(8 * 1024)),
            ResourceRecommendation::Hot
        );

        let rows = vec![ProcessRow {
            rss_mb: 4 * 1024,
            ..rows[0].clone()
        }];
        assert_eq!(
            classify_processes(&rows, 16, Some(128 * 1024)),
            ResourceRecommendation::Ok
        );
    }

    #[test]
    fn classifies_rig_leases_by_configured_concurrency_with_legacy_fallback() {
        assert_eq!(classify_rig_leases(2, Some(8)), ResourceRecommendation::Ok);
        assert_eq!(
            classify_rig_leases(6, Some(8)),
            ResourceRecommendation::Warm
        );
        assert_eq!(classify_rig_leases(8, Some(8)), ResourceRecommendation::Hot);
        assert_eq!(classify_rig_leases(0, None), ResourceRecommendation::Ok);
        assert_eq!(classify_rig_leases(1, None), ResourceRecommendation::Warm);
        assert_eq!(classify_rig_leases(2, None), ResourceRecommendation::Hot);
    }

    #[test]
    fn overall_recommendation_returns_hottest_signal() {
        assert_eq!(
            overall_recommendation(&[
                ResourceRecommendation::Ok,
                ResourceRecommendation::Hot,
                ResourceRecommendation::Warm,
            ]),
            ResourceRecommendation::Hot
        );
    }
}
