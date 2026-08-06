use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::{browser_evidence, engine::run_dir};
use crate::{Error, Result};

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
    /// Whether core seeds this sidecar with its empty default shape *before*
    /// the extension script runs, so a clean run that legitimately has nothing
    /// to report still leaves evidence that it measured.
    ///
    /// This is deliberately opt-in per key rather than "every declared
    /// sidecar", because for several sidecars the *absence* of the file is
    /// load-bearing: `test.results` missing is what makes the declared-parser
    /// stdout fallback engage (`test::run`), so pre-seeding `{}` there would
    /// silently suppress a real result path. Only sidecars whose empty shape is
    /// indistinguishable from "ran and found nothing" may be seeded. (#11123)
    pub seed_on_start: bool,
}

pub const REGISTRY: &[StructuredSidecarSchema] = &[
    StructuredSidecarSchema {
        key: "lint.findings",
        schema_version: "v1",
        path: run_dir::files::LINT_FINDINGS,
        producer: Some("lint"),
        shape: StructuredSidecarShape::Array,
        required_fields: &["message"],
        seed_on_start: true,
    },
    // Producer summaries travel beside the findings and share their lifecycle:
    // a lint run that produced no findings still owes the list of tools that
    // looked, so an empty array is the honest clean-pass value here too.
    StructuredSidecarSchema {
        key: "lint.producers",
        schema_version: "v1",
        path: run_dir::files::LINT_PRODUCERS,
        producer: Some("lint"),
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
        seed_on_start: true,
    },
    StructuredSidecarSchema {
        key: "test.results",
        schema_version: "v1",
        path: run_dir::files::TEST_RESULTS,
        producer: Some("test"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "test.failures",
        schema_version: "v1",
        path: run_dir::files::TEST_FAILURES,
        producer: Some("test"),
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "test.coverage",
        schema_version: "v1",
        path: run_dir::files::COVERAGE,
        producer: Some("test"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    // Duration is a first-class test fact, and it travels on its own key so a
    // slow suite can never be confused with a failing one. Optional inbound
    // enrichment: core derives durations from the runner's captured stdout, so
    // an extension that never writes this file loses nothing. Extensions that
    // can report richer timings than stdout carries write it here. (#10655)
    StructuredSidecarSchema {
        key: "test.durations",
        schema_version: "v1",
        path: run_dir::files::TEST_DURATIONS,
        producer: Some("test"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "bench.results",
        schema_version: "v1",
        path: run_dir::files::BENCH_RESULTS,
        producer: Some("bench"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "fuzz.results",
        schema_version: "v1",
        path: run_dir::files::FUZZ_RESULTS,
        producer: Some("fuzz"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "trace.results",
        schema_version: "v1",
        path: run_dir::files::TRACE_RESULTS,
        producer: Some("trace"),
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "trace.artifacts",
        schema_version: "v1",
        path: "artifacts",
        producer: Some("trace"),
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "resource.summary",
        schema_version: "v1",
        path: run_dir::files::RESOURCE_SUMMARY,
        producer: None,
        shape: StructuredSidecarShape::Object,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "producer.summary",
        schema_version: "v1",
        path: "producer-summary.json",
        producer: None,
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "findings",
        schema_version: "v1",
        path: "findings.json",
        producer: None,
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
        seed_on_start: false,
    },
    StructuredSidecarSchema {
        key: "annotations",
        schema_version: "v1",
        path: run_dir::files::ANNOTATIONS_DIR,
        producer: None,
        shape: StructuredSidecarShape::Array,
        required_fields: &[],
        seed_on_start: false,
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

impl StructuredSidecarSchema {
    /// The empty payload for this sidecar's declared shape — what core seeds
    /// before a run so a clean pass still leaves readable evidence.
    pub fn empty_payload(&self) -> Value {
        match self.shape {
            StructuredSidecarShape::Array => Value::Array(Vec::new()),
            StructuredSidecarShape::Object => Value::Object(serde_json::Map::new()),
        }
    }
}

/// The empty payload for a registry key's declared shape.
///
/// `None` for keys the registry does not know, which is the signal that core
/// has no contract for the sidecar and therefore must not invent one.
pub fn empty_payload(key: &str) -> Option<Value> {
    schema(key).map(StructuredSidecarSchema::empty_payload)
}

/// Whether core seeds this sidecar with its empty shape before a run.
///
/// Unknown keys are never seeded: core cannot know whether the absence of an
/// undeclared file is load-bearing to whoever reads it.
pub fn seeds_on_start(key: &str) -> bool {
    schema(key).is_some_and(|entry| entry.seed_on_start)
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

/// Project the legacy test parser's successful summary onto the declared
/// `test.failures` array contract. The parser writes aggregate counts beside
/// an empty failure list, while the sidecar itself represents only failures.
fn payload_for_validation<'a>(key: &str, payload: &'a Value) -> &'a Value {
    if key != "test.failures" {
        return payload;
    }

    let Some(object) = payload.as_object() else {
        return payload;
    };
    let Some(failures) = object.get("failures").filter(|value| value.is_array()) else {
        return payload;
    };
    let Some(total) = object.get("total").and_then(Value::as_u64) else {
        return payload;
    };
    let Some(passed) = object.get("passed").and_then(Value::as_u64) else {
        return payload;
    };

    if failures.as_array().is_some_and(Vec::is_empty) && total == passed {
        failures
    } else {
        payload
    }
}

/// What a post-run check of one declared sidecar file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarFileCheck {
    /// Core has no registry contract for this key, so there is nothing to
    /// judge. Declaring a vendor key is allowed; it simply buys no core
    /// guarantees, the same reading `empty_payload` and `seeds_on_start` take.
    Unknown,
    /// Nothing to validate: no file was written, the file is empty, or the
    /// declared path is a directory rather than a JSON document (`annotations`
    /// and `trace.artifacts` are directories).
    Absent,
    /// The file was read and its payload satisfies the registry contract.
    Valid,
}

/// Validate one declared sidecar's on-disk payload against the registry.
///
/// Until #11121 `validate_payload` had six callers, all of them parsers that
/// had *already* decided to read a specific sidecar. Nothing checked declared
/// sidecar output generically, so a runner could write a malformed
/// `bench.results` and every consumer downstream simply saw nothing.
///
/// Absence is deliberately not a violation. A missing file is load-bearing for
/// several keys (`test::run` treats a missing `test.results` as the signal to
/// parse counts from stdout), and failing a run for a file that was never
/// written is exactly the mistake that made the lint-findings evidence gate
/// fail passing lints. This fails only on a payload that is *present and
/// wrong*, which is an unambiguous contract violation by the producer.
pub fn validate_sidecar_file(key: &str, path: &Path) -> Result<SidecarFileCheck> {
    if schema(key).is_none() {
        return Ok(SidecarFileCheck::Unknown);
    }
    if !path.is_file() {
        return Ok(SidecarFileCheck::Absent);
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(SidecarFileCheck::Absent);
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(SidecarFileCheck::Absent);
    }

    let payload: Value = serde_json::from_str(trimmed).map_err(|err| {
        Error::validation_invalid_argument(
            "structured_sidecar",
            format!(
                "structured sidecar `{key}` at {} is not valid JSON: {err}",
                path.display()
            ),
            None,
            None,
        )
    })?;
    validate_payload(key, payload_for_validation(key, &payload))?;

    Ok(SidecarFileCheck::Valid)
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
            "test.durations",
            "bench.results",
            "trace.results",
        ] {
            assert!(keys.contains(&key), "missing registry key {key}");
            assert_eq!(default_schema_version(key), Some("v1"));
        }
    }

    /// The five keys that used to exist only in `homeboy-extension`'s shadow
    /// path/producer table. They resolved to a path but never to a schema
    /// version and were never reachable by `validate_payload`, so a declaration
    /// of any of them bought nothing. They are registry entries now. (#11121)
    #[test]
    fn registry_absorbed_the_extension_shadow_table_keys() {
        for (key, path, producer) in [
            ("lint.producers", "lint-producers.json", Some("lint")),
            ("test.coverage", "coverage.json", Some("test")),
            ("resource.summary", "resource-summary.json", None),
            ("producer.summary", "producer-summary.json", None),
            ("findings", "findings.json", None),
        ] {
            assert_eq!(default_path(key), Some(path), "path for {key}");
            assert_eq!(default_producer(key), producer, "producer for {key}");
            assert_eq!(
                default_schema_version(key),
                Some("v1"),
                "shadow-table keys carried no schema version before {key}"
            );
            validate_payload(key, &empty_payload(key).expect("registry key"))
                .unwrap_or_else(|err| panic!("empty payload for {key} must validate: {err}"));
        }
    }

    #[test]
    fn registry_keys_are_unique() {
        let mut keys: Vec<&str> = registry().iter().map(|entry| entry.key).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(total, keys.len(), "duplicate structured sidecar key");
    }

    #[test]
    fn empty_payload_matches_declared_shape() {
        for entry in registry() {
            let empty = entry.empty_payload();
            match entry.shape {
                StructuredSidecarShape::Array => assert!(empty.is_array(), "{}", entry.key),
                StructuredSidecarShape::Object => assert!(empty.is_object(), "{}", entry.key),
            }
        }
        assert_eq!(empty_payload("not.a.registry.key"), None);
    }

    /// Seeding is opt-in per key, and only for keys whose empty shape reads as
    /// "ran, found nothing". `test.results` must stay unseeded: its *absence*
    /// is what engages the declared-parser stdout fallback. (#11123)
    #[test]
    fn only_clean_pass_safe_sidecars_seed_on_start() {
        assert!(seeds_on_start("lint.findings"));
        assert!(seeds_on_start("lint.producers"));

        for key in [
            "test.results",
            "test.failures",
            "test.coverage",
            "bench.results",
            "trace.results",
            "annotations",
        ] {
            assert!(!seeds_on_start(key), "{key} must not be seeded by core");
        }

        assert!(!seeds_on_start("unknown.key"));
    }

    #[test]
    fn validates_known_valid_payloads() {
        validate_payload("lint.findings", &json!([{ "message": "lint failed" }])).unwrap();
        validate_payload("test.results", &json!({ "total": 2, "failed": 0 })).unwrap();
        validate_payload("test.failures", &json!([{ "message": "test failed" }])).unwrap();
        validate_payload("test.durations", &json!({ "measured_seconds": 12.5 })).unwrap();
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
        let err = validate_payload("lint.findings", &json!([{ "rule": "demo" }]))
            .expect_err("lint finding message is required");

        assert!(err.to_string().contains("message"));
    }

    #[test]
    fn accepts_test_failures_without_optional_fields() {
        validate_payload("test.failures", &json!([{ "test_id": "demo" }]))
            .expect("test failures may omit fields supplied by heterogeneous runners");
    }

    /// Post-run validation is not allowed to invent violations out of absence:
    /// a sidecar that was never written, or whose declared path is a directory,
    /// has nothing to check. (#11121)
    #[test]
    fn sidecar_file_validation_tolerates_absence_and_unknown_keys() {
        let dir = tempfile::tempdir().expect("temp dir");

        assert_eq!(
            validate_sidecar_file("lint.findings", &dir.path().join("never-written.json"))
                .expect("missing file"),
            SidecarFileCheck::Absent
        );

        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, "   \n").expect("empty file");
        assert_eq!(
            validate_sidecar_file("lint.findings", &empty).expect("empty file"),
            SidecarFileCheck::Absent
        );

        let annotations = dir.path().join("annotations");
        std::fs::create_dir(&annotations).expect("annotations dir");
        assert_eq!(
            validate_sidecar_file("annotations", &annotations).expect("directory sidecar"),
            SidecarFileCheck::Absent
        );

        // A key core has no contract for is not core's to judge, however
        // broken its contents are.
        let vendor = dir.path().join("vendor.json");
        std::fs::write(&vendor, "{ not json at all").expect("vendor file");
        assert_eq!(
            validate_sidecar_file("vendor.custom", &vendor).expect("unknown key"),
            SidecarFileCheck::Unknown
        );

        let valid = dir.path().join("lint-findings.json");
        std::fs::write(&valid, r#"[{"message":"boom"}]"#).expect("findings file");
        assert_eq!(
            validate_sidecar_file("lint.findings", &valid).expect("valid payload"),
            SidecarFileCheck::Valid
        );
    }

    /// A payload that is present and wrong is an unambiguous producer bug, and
    /// this is the seam that finally notices it. (#11121)
    #[test]
    fn sidecar_file_validation_rejects_present_but_invalid_payloads() {
        let dir = tempfile::tempdir().expect("temp dir");

        let malformed = dir.path().join("malformed.json");
        std::fs::write(&malformed, "{ malformed").expect("malformed file");
        let err =
            validate_sidecar_file("lint.findings", &malformed).expect_err("malformed JSON payload");
        assert!(err.to_string().contains("not valid JSON"), "{err}");

        let wrong_shape = dir.path().join("wrong-shape.json");
        std::fs::write(&wrong_shape, r#"{"message":"boom"}"#).expect("wrong shape file");
        let err = validate_sidecar_file("lint.findings", &wrong_shape)
            .expect_err("lint findings must be an array");
        assert!(err.to_string().contains("JSON array"), "{err}");

        let missing_field = dir.path().join("missing-field.json");
        std::fs::write(&missing_field, r#"[{"rule":"demo"}]"#).expect("missing field file");
        let err = validate_sidecar_file("lint.findings", &missing_field)
            .expect_err("lint finding message is required");
        assert!(err.to_string().contains("message"), "{err}");

        // Browser-evidence keys keep their extra validation on this path too.
        let bench = dir.path().join("bench-results.json");
        std::fs::write(&bench, r#"{"network":"not-an-array"}"#).expect("bench file");
        let err =
            validate_sidecar_file("bench.results", &bench).expect_err("browser evidence shape");
        assert!(err.to_string().contains("network"), "{err}");
    }

    #[test]
    fn sidecar_file_validation_normalizes_successful_legacy_test_failure_summaries() {
        let dir = tempfile::tempdir().expect("temp dir");
        let failures = dir.path().join("test-failures.json");
        std::fs::write(
            &failures,
            r#"{"failures":[],"total":10,"passed":10,"metadata":{"assertions":42}}"#,
        )
        .expect("test failure summary");

        assert_eq!(
            validate_sidecar_file("test.failures", &failures)
                .expect("successful legacy summary is an empty failure sidecar"),
            SidecarFileCheck::Valid
        );
    }

    #[test]
    fn sidecar_file_validation_rejects_invalid_test_failure_summary_shapes() {
        let dir = tempfile::tempdir().expect("temp dir");

        for (name, payload) in [
            ("not-an-array", r#"{"failures":{},"total":10,"passed":10}"#),
            ("not-successful", r#"{"failures":[],"total":10,"passed":9}"#),
        ] {
            let path = dir.path().join(format!("{name}.json"));
            std::fs::write(&path, payload).expect("invalid test failure summary");
            let error = validate_sidecar_file("test.failures", &path)
                .expect_err("only successful empty legacy summaries are normalized");
            assert!(error.to_string().contains("JSON array"), "{error}");
        }
    }
}
