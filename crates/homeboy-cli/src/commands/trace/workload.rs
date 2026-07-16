use std::path::Path;

pub(super) fn trace_workload_scenario_id(path: &str) -> String {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    if let Some((stem, _)) = file_name.split_once(".trace.") {
        return stem.to_string();
    }
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
        .to_string()
}
