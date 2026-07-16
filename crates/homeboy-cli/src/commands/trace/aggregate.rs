use homeboy::core::extension::trace as extension_trace;

#[derive(Clone)]
pub(super) struct TraceAggregateSpanSample {
    pub(super) duration_ms: u64,
    pub(super) run_index: usize,
    pub(super) artifact_path: String,
}

#[derive(Clone)]
pub(super) struct TraceAggregateMetricSample {
    pub(super) value: f64,
    pub(super) run_index: usize,
    pub(super) artifact_path: String,
}

pub(super) fn aggregate_span(
    id: String,
    samples: Vec<TraceAggregateSpanSample>,
    failures: usize,
) -> extension_trace::TraceAggregateSpanOutput {
    let max_sample: Option<TraceAggregateSpanSample> =
        samples.iter().fold(None, |max, sample| match max {
            Some(current) if current.duration_ms >= sample.duration_ms => Some(current),
            _ => Some(sample.clone()),
        });
    let sample_outputs = samples
        .iter()
        .map(|sample| extension_trace::TraceAggregateSpanSampleOutput {
            run_index: sample.run_index,
            duration_ms: sample.duration_ms,
            artifact_path: sample.artifact_path.clone(),
        })
        .collect::<Vec<_>>();
    let mut durations = samples
        .iter()
        .map(|sample| sample.duration_ms)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    let n = durations.len();
    let avg_ms = if n == 0 {
        None
    } else {
        Some(durations.iter().sum::<u64>() as f64 / n as f64)
    };
    extension_trace::TraceAggregateSpanOutput {
        id,
        n,
        min_ms: durations.first().copied(),
        median_ms: median(&durations),
        avg_ms,
        stddev_ms: stddev(&durations, avg_ms),
        p75_ms: percentile(&durations, 75, 4),
        p90_ms: percentile(&durations, 90, 10),
        p95_ms: percentile(&durations, 95, 20),
        max_ms: durations.last().copied(),
        max_run_index: max_sample.as_ref().map(|sample| sample.run_index),
        max_artifact_path: max_sample.map(|sample| sample.artifact_path),
        failures,
        samples: sample_outputs,
        metadata: None,
    }
}

pub(super) fn aggregate_metric(
    id: String,
    samples: Vec<TraceAggregateMetricSample>,
) -> extension_trace::TraceAggregateMetricOutput {
    let sample_outputs = samples
        .iter()
        .map(|sample| extension_trace::TraceAggregateMetricSampleOutput {
            run_index: sample.run_index,
            value: sample.value,
            artifact_path: sample.artifact_path.clone(),
        })
        .collect::<Vec<_>>();
    let mut values = samples
        .iter()
        .map(|sample| sample.value)
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    extension_trace::TraceAggregateMetricOutput {
        id,
        n: values.len(),
        min: values.first().copied(),
        median: median_f64(&values),
        max: values.last().copied(),
        samples: sample_outputs,
    }
}

fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let midpoint = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[midpoint])
    } else {
        Some((values[midpoint - 1] + values[midpoint]) / 2)
    }
}

fn median_f64(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let midpoint = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[midpoint])
    } else {
        Some((values[midpoint - 1] + values[midpoint]) / 2.0)
    }
}

fn stddev(values: &[u64], avg: Option<f64>) -> Option<f64> {
    let avg = avg?;
    if values.is_empty() {
        return None;
    }
    let variance = values
        .iter()
        .map(|value| {
            let delta = *value as f64 - avg;
            delta * delta
        })
        .sum::<f64>()
        / values.len() as f64;
    Some(variance.sqrt())
}

fn percentile(values: &[u64], percentile: usize, min_samples: usize) -> Option<u64> {
    if values.len() < min_samples {
        return None;
    }
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values.get(index).copied()
}
