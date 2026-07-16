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

#[cfg(unix)]
fn load_averages() -> Option<[f64; 3]> {
    let mut values = [0.0_f64; 3];
    let count = unsafe { libc::getloadavg(values.as_mut_ptr(), values.len() as i32) };
    if count == 3 {
        Some(values)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn load_averages() -> Option<[f64; 3]> {
    None
}
