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

pub(super) fn classify_processes(rows: &[ProcessRow]) -> ResourceRecommendation {
    if rows
        .iter()
        .any(|row| row.cpu_percent >= 200.0 || row.rss_mb >= 4096)
    {
        ResourceRecommendation::Hot
    } else if rows
        .iter()
        .any(|row| row.cpu_percent >= 100.0 || row.rss_mb >= 2048)
    {
        ResourceRecommendation::Warm
    } else {
        ResourceRecommendation::Ok
    }
}

pub(super) fn classify_rig_leases(active_count: usize) -> ResourceRecommendation {
    match active_count {
        0 => ResourceRecommendation::Ok,
        1 => ResourceRecommendation::Warm,
        _ => ResourceRecommendation::Hot,
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
    fn classifies_processes_by_hot_cpu_or_rss_rows() {
        let rows = vec![ProcessRow {
            pid: 1,
            cpu_percent: 25.0,
            rss_mb: 512,
            command: "homeboy".to_string(),
            args: "homeboy bench".to_string(),
        }];
        assert_eq!(classify_processes(&rows), ResourceRecommendation::Ok);

        let rows = vec![ProcessRow {
            cpu_percent: 101.0,
            ..rows[0].clone()
        }];
        assert_eq!(classify_processes(&rows), ResourceRecommendation::Warm);

        let rows = vec![ProcessRow {
            cpu_percent: 201.0,
            ..rows[0].clone()
        }];
        assert_eq!(classify_processes(&rows), ResourceRecommendation::Hot);
    }

    #[test]
    fn classifies_rig_leases_by_active_count() {
        assert_eq!(classify_rig_leases(0), ResourceRecommendation::Ok);
        assert_eq!(classify_rig_leases(1), ResourceRecommendation::Warm);
        assert_eq!(classify_rig_leases(2), ResourceRecommendation::Hot);
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
