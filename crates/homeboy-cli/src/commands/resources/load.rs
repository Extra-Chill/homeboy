use super::classification::classify_load;
use super::{round1, LoadSummary};

pub(super) fn collect_load_summary() -> LoadSummary {
    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let averages = load_averages();
    let recommendation = classify_load(averages, cpu_count);

    LoadSummary {
        one: averages.map(|values| round1(values[0])),
        five: averages.map(|values| round1(values[1])),
        fifteen: averages.map(|values| round1(values[2])),
        cpu_count,
        recommendation,
    }
}

#[cfg(any(test, feature = "test-support"))]
fn test_load_averages() -> Option<[f64; 3]> {
    let raw = std::env::var("HOMEBOY_TEST_LOAD_AVERAGES").ok()?;
    let mut values = raw.split(',').map(str::parse::<f64>);
    Some([
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ])
}

fn load_averages() -> Option<[f64; 3]> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(values) = test_load_averages() {
        return Some(values);
    }
    homeboy_engine_primitives::host_telemetry::load_average()
        .map(|load| [load.one, load.five, load.fifteen])
}
