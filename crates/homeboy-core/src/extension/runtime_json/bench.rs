//! BenchResults shaping behind `bench-helper.sh`.

use super::json::Json;
use std::fs;
use std::io::Write;
use std::path::Path;

/// `{"path":..,"kind"?:..,"label"?:..}` artifact reference.
pub fn artifact_ref(path: &str, kind: &str, label: &str) -> Json {
    let mut entries = vec![("path".to_string(), Json::string(path))];
    if !kind.is_empty() {
        entries.push(("kind".to_string(), Json::string(kind)));
    }
    if !label.is_empty() {
        entries.push(("label".to_string(), Json::string(label)));
    }
    Json::Object(entries)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Append one responsiveness ping line to `file`.
pub fn responsiveness_ping(file: &str, started_ms: i64, label: &str) -> Result<(), String> {
    let now = chrono::Utc::now();
    let mut ping = vec![
        (
            "at".to_string(),
            Json::String(now.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        ),
        (
            "t_ms".to_string(),
            Json::int((now.timestamp_millis() - started_ms).max(0)),
        ),
    ];
    if !label.is_empty() {
        ping.push(("label".to_string(), Json::string(label)));
    }
    if let Some(parent) = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut handle = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|error| format!("open {file}: {error}"))?;
    writeln!(handle, "{}", Json::Object(ping).to_compact_ascii())
        .map_err(|error| format!("write {file}: {error}"))
}

#[derive(Debug, Default, Clone)]
pub struct BenchResultsRequest {
    pub inventory: bool,
    pub results_file: String,
    pub component_id: String,
    pub iterations: i64,
    pub metadata_json: String,
    pub metric_groups_json: String,
    pub span_definitions_json: String,
    pub artifacts_json: String,
    pub timeline_json: String,
    pub extras_file: String,
    /// `id=payload_path=file=source` scenario records.
    pub records: Vec<String>,
}

/// Build the BenchResults envelope. Returns the envelope plus warnings for
/// scenarios that emitted no timings (they are dropped).
pub fn results_envelope(request: &BenchResultsRequest) -> Result<(Json, Vec<String>), String> {
    let mut metadata = json_arg("metadata", &request.metadata_json, Json::Object(Vec::new()))?;
    let mut metric_groups = json_arg(
        "metric_groups",
        &request.metric_groups_json,
        Json::Object(Vec::new()),
    )?;
    let mut span_definitions = json_arg(
        "span_definitions",
        &request.span_definitions_json,
        Json::Object(Vec::new()),
    )?;
    let mut artifacts = json_arg(
        "artifacts",
        &request.artifacts_json,
        Json::Object(Vec::new()),
    )?;
    let mut timeline = json_arg("timeline", &request.timeline_json, Json::Array(Vec::new()))?;
    if !request.extras_file.is_empty() {
        let extras = load_json(&request.extras_file)?;
        for (key, slot) in [
            ("metadata", &mut metadata),
            ("metric_groups", &mut metric_groups),
            ("span_definitions", &mut span_definitions),
            ("artifacts", &mut artifacts),
            ("timeline", &mut timeline),
        ] {
            if let Some(value) = extras.get(key) {
                *slot = value.clone();
            }
        }
    }

    let mut warnings = Vec::new();
    let mut scenarios = Vec::new();
    for record in request.records.iter().filter(|record| !record.is_empty()) {
        if let Some(scenario) = scenario(request, record, &mut warnings)? {
            scenarios.push(scenario);
        }
    }

    let mut envelope = Json::Object(vec![
        ("component_id".into(), Json::string(&request.component_id)),
        (
            "iterations".into(),
            Json::int(if request.inventory {
                0
            } else {
                request.iterations
            }),
        ),
        ("scenarios".into(), Json::Array(scenarios)),
    ]);
    for (key, value) in [
        ("metadata", metadata),
        ("metric_groups", metric_groups),
        ("span_definitions", span_definitions),
        ("artifacts", artifacts),
        ("timeline", timeline),
    ] {
        if value.is_truthy() {
            envelope.insert(key, value);
        }
    }
    Ok((envelope, warnings))
}

/// Build and write the envelope as two-space indented JSON.
pub fn write_results(request: &BenchResultsRequest) -> Result<Vec<String>, String> {
    let (envelope, warnings) = results_envelope(request)?;
    let target = Path::new(&request.results_file);
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut text = envelope.to_pretty_ascii();
    text.push('\n');
    fs::write(target, text).map_err(|error| format!("write {}: {error}", target.display()))?;
    Ok(warnings)
}

fn scenario(
    request: &BenchResultsRequest,
    record: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<Json>, String> {
    let mut parts = record.splitn(4, '=');
    let id = parts.next().unwrap_or_default();
    let mut payload_path = parts.next().unwrap_or_default();
    let mut file_value = parts.next().unwrap_or_default();
    let mut source = parts.next().unwrap_or_default();
    // Inventory records may omit the payload: `id=file=source`.
    if request.inventory
        && source.is_empty()
        && !payload_path.is_empty()
        && payload_path != "null"
        && !file_value.is_empty()
    {
        (payload_path, file_value, source) = ("", payload_path, file_value);
    }

    let payload = if !payload_path.is_empty() && payload_path != "null" {
        let payload = load_json(payload_path)?;
        if !matches!(payload, Json::Object(_)) {
            return Err(format!(
                "bench payload must be a JSON object: {payload_path}"
            ));
        }
        payload
    } else {
        Json::Object(Vec::new())
    };

    let mut scenario = Json::Object(vec![("id".into(), Json::string(id))]);
    if request.inventory {
        scenario.insert("iterations", Json::int(0));
        scenario.insert("default_iterations", Json::int(request.iterations));
        scenario.insert(
            "tags",
            payload
                .get("tags")
                .cloned()
                .unwrap_or(Json::Array(Vec::new())),
        );
        scenario.insert(
            "metrics",
            payload
                .get("metrics")
                .cloned()
                .unwrap_or(Json::Object(Vec::new())),
        );
    } else {
        let mut timings_ms = match payload.get("timings_ns") {
            Some(Json::Array(values)) => values
                .iter()
                .map(|value| number(value).map(|ns| ns / 1_000_000.0))
                .collect::<Result<Vec<_>, _>>()?,
            _ => Vec::new(),
        };
        timings_ms.sort_by(f64::total_cmp);
        let count = timings_ms.len();
        if count == 0 {
            warnings.push(format!("WORKLOAD_WARN: {id} emitted no timings"));
            return Ok(None);
        }
        let mut metrics = Json::Object(vec![
            (
                "mean_ms".into(),
                Json::float(timings_ms.iter().sum::<f64>() / count as f64),
            ),
            (
                "p50_ms".into(),
                Json::float(percentile_r7(&timings_ms, 0.50)),
            ),
            (
                "p95_ms".into(),
                Json::float(percentile_r7(&timings_ms, 0.95)),
            ),
            (
                "p99_ms".into(),
                Json::float(percentile_r7(&timings_ms, 0.99)),
            ),
            ("min_ms".into(), Json::float(timings_ms[0])),
            ("max_ms".into(), Json::float(timings_ms[count - 1])),
        ]);
        if let Some(Json::Object(extra)) = payload.get("metrics") {
            for (key, value) in extra {
                if matches!(value, Json::Number(_) | Json::Bool(_)) {
                    metrics.insert(key, value.clone());
                }
            }
        }
        scenario.insert("iterations", Json::int(count as i64));
        scenario.insert("metrics", metrics);
    }

    if !file_value.is_empty() && file_value != "null" {
        scenario.insert("file", Json::string(file_value));
    } else if let Some(file @ Json::String(_)) = payload.get("file") {
        scenario.insert("file", file.clone());
    }
    if !source.is_empty() {
        scenario.insert("source", Json::string(source));
    } else if let Some(value @ Json::String(_)) = payload.get("source") {
        scenario.insert("source", value.clone());
    }
    for key in ["metadata", "artifacts"] {
        if let Some(value @ Json::Object(_)) = payload.get(key) {
            scenario.insert(key, value.clone());
        }
    }
    if let Some(peak) = payload.get("peak_rss_bytes") {
        let peak = number(peak)?.trunc() as i64;
        scenario.insert(
            "memory",
            Json::Object(vec![("peak_bytes".into(), Json::int(peak))]),
        );
    }
    Ok(Some(scenario))
}

/// R-7 percentile, the runner contract used by Homeboy BenchResults
/// producers: rank = p * (n - 1), linear interpolation between neighbours.
pub fn percentile_r7(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    match n {
        0 => 0.0,
        1 => sorted[0],
        _ => {
            let rank = p * (n - 1) as f64;
            let lo = rank as usize;
            let hi = (lo + usize::from(rank > lo as f64)).min(n - 1);
            if lo == hi {
                sorted[lo]
            } else {
                let frac = rank - lo as f64;
                sorted[lo] * (1.0 - frac) + sorted[hi] * frac
            }
        }
    }
}

fn number(value: &Json) -> Result<f64, String> {
    match value {
        Json::Number(number) => number
            .as_f64()
            .ok_or_else(|| format!("bench value is not numeric: {number}")),
        Json::Bool(value) => Ok(f64::from(u8::from(*value))),
        Json::String(raw) => raw
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("bench value is not numeric: {raw}")),
        other => Err(format!(
            "bench value is not numeric: {}",
            other.to_compact()
        )),
    }
}

fn json_arg(name: &str, raw: &str, fallback: Json) -> Result<Json, String> {
    if raw.is_empty() {
        return Ok(fallback);
    }
    Json::parse(raw).map_err(|error| format!("{name} must be valid JSON: {error}"))
}

fn load_json(path: &str) -> Result<Json, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    Json::parse(&raw).map_err(|error| format!("{path}: {error}"))
}
