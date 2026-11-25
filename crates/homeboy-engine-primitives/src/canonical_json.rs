//! Canonical JSON: a single, order-independent serialization contract.
//!
//! Multiple subsystems compute stable digests over JSON payloads (Lab staging
//! recipe digests, private run-attachment digests, component/deploy evidence
//! digests). Each independently reimplemented "recursively sort object keys"
//! canonicalization under a different name (#9759). Divergence between those
//! copies would silently change digests, so the algorithm is consolidated here
//! as the one canonicalization contract all consumers share.
//!
//! The contract: object keys are sorted recursively; arrays preserve order but
//! their elements are canonicalized; scalars pass through unchanged. This is
//! byte-for-byte identical to the previous per-crate copies.

use serde::Serialize;
use serde_json::Value;

/// Recursively canonicalize a JSON value: object keys are sorted (ascending,
/// by key string), array order is preserved, scalars pass through. Applied
/// before serialization so `serde_json::to_vec` produces a stable, order-
/// independent byte string suitable for hashing.
pub fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(values) => {
            let mut fields: Vec<_> = values.into_iter().collect();
            fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        value => value,
    }
}

/// Serialize any `Serialize` value to its canonical JSON byte string.
///
/// Equivalent to `serde_json::to_vec(&canonical_json(serde_json::to_value(v)?))`.
/// Consumers hash the returned bytes to produce a stable digest.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> serde_json::Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    serde_json::to_vec(&canonical_json(value))
}

#[cfg(test)]
mod tests {
    use super::{canonical_json, canonical_json_bytes};
    use serde_json::json;

    #[test]
    fn sorts_object_keys_recursively_and_preserves_arrays_and_scalars() {
        let input = json!({
            "b": 1.5,
            "a": { "z": true, "y": null },
            "c": [ 3, 1, { "n": "s", "m": 1 } ]
        });
        let rendered = serde_json::to_string(&canonical_json(input)).unwrap();
        assert_eq!(
            rendered,
            r#"{"a":{"y":null,"z":true},"b":1.5,"c":[3,1,{"m":1,"n":"s"}]}"#
        );
    }

    #[test]
    fn bytes_are_independent_of_key_insertion_order() {
        // The property every digest consumer relies on.
        let a = json!({ "one": 1, "two": { "b": 2, "a": 1 }, "three": [3, 2, 1] });
        let b = json!({ "three": [3, 2, 1], "two": { "a": 1, "b": 2 }, "one": 1 });
        assert_eq!(
            canonical_json_bytes(&a).unwrap(),
            canonical_json_bytes(&b).unwrap()
        );
    }
}
