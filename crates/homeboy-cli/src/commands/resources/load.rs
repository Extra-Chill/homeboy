use super::classification::classify_load;
use super::{round1, LoadSummary};
use homeboy::core::local_dispatch_admission::{host_cpu_count, host_load_averages};

pub(super) fn collect_load_summary() -> LoadSummary {
    let cpu_count = host_cpu_count();
    let averages = host_load_averages();
    let recommendation = classify_load(averages, cpu_count);

    LoadSummary {
        one: averages.map(|values| round1(values[0])),
        five: averages.map(|values| round1(values[1])),
        fifteen: averages.map(|values| round1(values[2])),
        cpu_count,
        recommendation,
    }
}
