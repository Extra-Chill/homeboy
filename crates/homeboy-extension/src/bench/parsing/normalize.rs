//! JSON value preprocessing applied before deserializing into `BenchResults`.
//!
//! These helpers operate on the raw `serde_json::Value` to keep the strict
//! `BenchResults` deserializer tolerant of forward-compatible extension output:
//! they drop non-contract envelope keys, filter scenarios by selection, fold
//! extension sample metrics into the canonical shape, hoist diagnostic
//! `source` fields into metadata, and strip inline artifact payloads that lack
//! a pointer field.

pub(super) fn normalize_inline_artifact_payloads(value: &mut serde_json::Value) {
    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for scenario in scenarios {
        normalize_artifact_map(scenario.get_mut("artifacts"));

        let Some(runs) = scenario
            .get_mut("runs")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for run in runs {
            normalize_artifact_map(run.get_mut("artifacts"));
        }
    }
}

fn normalize_artifact_map(value: Option<&mut serde_json::Value>) {
    let Some(artifacts) = value.and_then(serde_json::Value::as_object_mut) else {
        return;
    };

    artifacts.retain(|_, artifact| artifact_has_pointer_field(artifact));
}

fn artifact_has_pointer_field(artifact: &serde_json::Value) -> bool {
    let Some(object) = artifact.as_object() else {
        return false;
    };
    [
        "path",
        "url",
        "public_url",
        "preview_url",
        "viewer_url",
        "local_url",
        "observation_artifact_id",
    ]
    .iter()
    .any(|field| object.contains_key(*field))
}

pub(super) fn normalize_diagnostic_producer_sources(value: &mut serde_json::Value) {
    normalize_diagnostic_array(value.get_mut("diagnostics"));

    if let Some(run_metadata) = value.get_mut("run_metadata") {
        normalize_diagnostic_array(run_metadata.get_mut("diagnostics"));
    }

    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for scenario in scenarios {
        normalize_diagnostic_array(scenario.get_mut("diagnostics"));
        let Some(runs) = scenario
            .get_mut("runs")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for run in runs {
            normalize_diagnostic_array(run.get_mut("diagnostics"));
        }
    }
}

fn normalize_diagnostic_array(value: Option<&mut serde_json::Value>) {
    let Some(diagnostics) = value.and_then(serde_json::Value::as_array_mut) else {
        return;
    };

    for diagnostic in diagnostics {
        let Some(object) = diagnostic.as_object_mut() else {
            continue;
        };
        let Some(source) = object.remove("source") else {
            continue;
        };
        match source {
            serde_json::Value::String(source) => {
                let metadata_key = if object.contains_key("metadata") {
                    "metadata"
                } else if object.contains_key("details") {
                    "details"
                } else {
                    "metadata"
                };
                let metadata = object
                    .entry(metadata_key.to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(metadata_object) = metadata.as_object_mut() {
                    metadata_object.insert(
                        "producer_source".to_string(),
                        serde_json::Value::String(source),
                    );
                }
            }
            other => {
                object.insert("source".to_string(), other);
            }
        }
    }
}

pub(super) fn filter_value_scenarios_by_ids(
    value: &mut serde_json::Value,
    scenario_ids: &[String],
) {
    if scenario_ids.is_empty() {
        return;
    }

    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    scenarios.retain(|scenario| {
        scenario
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| scenario_ids.iter().any(|selected| selected == id))
    });
}

pub(super) fn normalize_extension_sample_metrics(value: &mut serde_json::Value) {
    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for scenario in scenarios {
        if let Some(object) = scenario.as_object_mut() {
            if object
                .get("provenance")
                .is_some_and(|value| !is_bench_provenance_contract(value))
            {
                object.remove("provenance");
            }
        }
        let Some(metrics) = scenario
            .get_mut("metrics")
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };

        let mut normalized = serde_json::Map::new();
        let mut distributions = serde_json::Map::new();
        for (name, metric) in std::mem::take(metrics) {
            if metric.is_number() {
                normalized.insert(name, metric);
                continue;
            }

            let Some(samples) = metric.get("samples").and_then(serde_json::Value::as_object) else {
                normalized.insert(name, metric);
                continue;
            };
            if let Some(mean) = samples.get("mean").and_then(serde_json::Value::as_f64) {
                if let Some(number) = serde_json::Number::from_f64(mean) {
                    normalized.insert(name.clone(), serde_json::Value::Number(number));
                }
            }
            if let Some(values) = samples.get("values").and_then(serde_json::Value::as_array) {
                distributions.insert(name, serde_json::Value::Array(values.clone()));
            }
        }

        if !distributions.is_empty() {
            normalized.insert(
                "distributions".to_string(),
                serde_json::Value::Object(distributions),
            );
        }
        *metrics = normalized;
    }
}

pub(super) fn is_bench_provenance_contract(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.contains_key("links") || object.contains_key("labels"))
}
