use serde::Serialize;
use serde_json::Value;

use crate::core::{browser_evidence, engine::run_dir};
use crate::core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredSidecarShape {
    Array,
    Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StructuredSidecarSchema {
    pub key: &'static str,
    pub schema_version: &'static str,
    pub path: &'static str,
    pub producer: Option<&'static str>,
    pub shape: StructuredSidecarShape,
    pub required_fields: &'static [&'static str],
}

pub const REGISTRY: &[StructuredSidecarSchema] = &[
    StructuredSidecarSchema {
        key: "lint.findings",
        schema_version: "v1",
        path: run_dir::files::LINT_FINDINGS,
        producer: Some("lint"),
        shape: StructuredSidecarShape::Array,
        required_fields: &["message"],
    },
    StructuredSidecarSchema {
        key: "test.results",
        schema_version: "v1",
        path: run_dir::files::TEST_RESULTS,
        producer: Some("test"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
    },
    StructuredSidecarSchema {
        key: "test.failures",
        schema_version: "v1",
        path: run_dir::files::TEST_FAILURES,
        producer: Some("test"),
        shape: StructuredSidecarShape::Array,
        required_fields: &["message"],
    },
    StructuredSidecarSchema {
        key: "bench.results",
        schema_version: "v1",
        path: run_dir::files::BENCH_RESULTS,
        producer: Some("bench"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
    },
    StructuredSidecarSchema {
        key: "fuzz.results",
        schema_version: "v1",
        path: run_dir::files::FUZZ_RESULTS,
        producer: Some("fuzz"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
    },
    StructuredSidecarSchema {
        key: "trace.results",
        schema_version: "v1",
        path: run_dir::files::TRACE_RESULTS,
        producer: Some("trace"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
    },
    StructuredSidecarSchema {
        key: "trace.artifacts",
        schema_version: "v1",
        path: "artifacts",
        producer: Some("trace"),
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
    },
    StructuredSidecarSchema {
        key: "annotations",
        schema_version: "v1",
        path: run_dir::files::ANNOTATIONS_DIR,
        producer: None,
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
    },
];

pub fn registry() -> &'static [StructuredSidecarSchema] {
    REGISTRY
}

pub fn schema(key: &str) -> Option<&'static StructuredSidecarSchema> {
    REGISTRY.iter().find(|entry| entry.key == key)
}

pub fn default_path(key: &str) -> Option<&'static str> {
    schema(key).map(|entry| entry.path)
}

pub fn default_producer(key: &str) -> Option<&'static str> {
    schema(key).and_then(|entry| entry.producer)
}

pub fn default_schema_version(key: &str) -> Option<&'static str> {
    schema(key).map(|entry| entry.schema_version)
}

pub fn validate_payload(key: &str, payload: &Value) -> Result<()> {
    let schema = schema(key).ok_or_else(|| {
        Error::validation_invalid_argument(
            "structured_sidecar",
            format!("unknown structured sidecar key `{key}`"),
            None,
            Some(vec![format!(
                "Known keys: {}",
                REGISTRY
                    .iter()
                    .map(|entry| entry.key)
                    .collect::<Vec<_>>()
                    .join(", ")
            )]),
        )
    })?;

    match schema.shape {
        StructuredSidecarShape::Array => validate_array_payload(schema, payload),
        StructuredSidecarShape::Object => validate_object_payload(schema, payload),
    }?;

    match key {
        "bench.results" => browser_evidence::validate_bench_results_payload(payload),
        "trace.results" => browser_evidence::validate_trace_results_payload(payload),
        _ => Ok(()),
    }
}

fn validate_array_payload(schema: &StructuredSidecarSchema, payload: &Value) -> Result<()> {
    let Some(items) = payload.as_array() else {
        return Err(shape_error(schema, "JSON array"));
    };

    for (index, item) in items.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(Error::validation_invalid_argument(
                "structured_sidecar",
                format!(
                    "structured sidecar `{}` item {index} must be a JSON object",
                    schema.key
                ),
                None,
                None,
            ));
        };

        for field in schema.required_fields {
            if !object.contains_key(*field) {
                return Err(Error::validation_invalid_argument(
                    "structured_sidecar",
                    format!(
                        "structured sidecar `{}` item {index} is missing required field `{field}`",
                        schema.key
                    ),
                    None,
                    None,
                ));
            }
        }
    }

    Ok(())
}

fn validate_object_payload(schema: &StructuredSidecarSchema, payload: &Value) -> Result<()> {
    let Some(object) = payload.as_object() else {
        return Err(shape_error(schema, "JSON object"));
    };

    for field in schema.required_fields {
        if !object.contains_key(*field) {
            return Err(Error::validation_invalid_argument(
                "structured_sidecar",
                format!(
                    "structured sidecar `{}` is missing required field `{field}`",
                    schema.key
                ),
                None,
                None,
            ));
        }
    }

    Ok(())
}

fn shape_error(schema: &StructuredSidecarSchema, expected: &str) -> Error {
    Error::validation_invalid_argument(
        "structured_sidecar",
        format!(
            "structured sidecar `{}` must be a {expected} for schema {}",
            schema.key, schema.schema_version
        ),
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_contains_current_core_sidecars() {
        let keys: Vec<&str> = registry().iter().map(|entry| entry.key).collect();

        for key in [
            "lint.findings",
            "test.results",
            "test.failures",
            "bench.results",
            "trace.results",
        ] {
            assert!(keys.contains(&key), "missing registry key {key}");
            assert_eq!(default_schema_version(key), Some("v1"));
        }
    }

    #[test]
    fn validates_known_valid_payloads() {
        validate_payload("lint.findings", &json!([{ "message": "lint failed" }])).unwrap();
        validate_payload("test.results", &json!({ "total": 2, "failed": 0 })).unwrap();
        validate_payload("test.failures", &json!([{ "message": "test failed" }])).unwrap();
        validate_payload("bench.results", &json!({ "results": [] })).unwrap();
        validate_payload("trace.results", &json!({ "runs": [] })).unwrap();
        validate_payload("bench.results", &json!({ "browser_profiles": [] })).unwrap();
        validate_payload(
            "trace.results",
            &json!({ "timeline": [], "assertions": [] }),
        )
        .unwrap();
    }

    #[test]
    fn rejects_invalid_payload_shapes() {
        let err = validate_payload("lint.findings", &json!({ "message": "wrong" }))
            .expect_err("lint findings must be an array");

        assert!(err.to_string().contains("JSON array"));
    }

    #[test]
    fn rejects_missing_required_array_fields() {
        let err = validate_payload("test.failures", &json!([{ "test_id": "demo" }]))
            .expect_err("test failure message is required");

        assert!(err.to_string().contains("message"));
    }
}

#[cfg(test)]
#[path = "../../tests/core/extension/structured_sidecar_test.rs"]
mod structured_sidecar_test;
