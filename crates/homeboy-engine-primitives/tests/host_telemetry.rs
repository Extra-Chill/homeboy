#![cfg(any(target_os = "linux", target_os = "macos"))]

use homeboy_engine_primitives::host_telemetry::{load_average, probe_system_memory, LoadAverage};

#[test]
fn supported_hosts_publish_portable_load_samples() {
    let load = load_average().expect("supported hosts provide load averages");
    for value in [load.one, load.five, load.fifteen] {
        assert!(value.is_finite() && value >= 0.0);
    }
    let json = serde_json::to_value(&load).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "one": load.one, "five": load.five, "fifteen": load.fifteen,
        })
    );
    assert_eq!(serde_json::from_value::<LoadAverage>(json).unwrap(), load);
}

#[test]
fn supported_hosts_publish_memory_in_bytes() {
    let memory = probe_system_memory().expect("supported hosts provide physical memory");
    assert!(memory.total_bytes > 0);
    let available = memory
        .available_bytes
        .expect("available memory is measurable");
    assert!(available <= memory.total_bytes);
    #[cfg(target_os = "linux")]
    assert!(memory.swap_used_bytes.is_some());
}
