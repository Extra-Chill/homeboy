//! Artifact-reference URI schemes.
//!
//! The stable string schemes that identify how an artifact path should be
//! interpreted across process boundaries:
//!
//! - `homeboy://` — a homeboy-native artifact reference.
//! - `runner-artifact://` — an artifact held by a runner (fetched on demand).
//! - `metadata-only:` — a reference that carries only metadata, no payload.
//!
//! These are protocol constants shared by the runner/Lab/daemon execution
//! surface (`homeboy-core::artifact_ref` / `execution_contract`), the CLI command
//! contract, and the audit engine's artifact-portability detector. They live in
//! `homeboy-engine-primitives` — the slim shared base — so consumers depend on
//! the primitives layer rather than reaching up into `homeboy-core` (or each
//! other) for a handful of scheme strings and prefix checks.

/// Scheme for homeboy-native artifact references.
pub const HOMEBOY_REF_SCHEME: &str = "homeboy://";

/// Scheme for artifacts held by a runner (materialized on demand).
pub const RUNNER_ARTIFACT_REF_SCHEME: &str = "runner-artifact://";

/// Scheme for references that carry only metadata (no payload).
pub const METADATA_ONLY_REF_SCHEME: &str = "metadata-only:";

/// Whether `path` is a runner-artifact reference (`runner-artifact://…`).
pub fn is_runner_artifact_ref(path: &str) -> bool {
    path.starts_with(RUNNER_ARTIFACT_REF_SCHEME)
}

/// Whether `path` is a metadata-only reference (`metadata-only:…`).
pub fn is_metadata_only_ref(path: &str) -> bool {
    path.starts_with(METADATA_ONLY_REF_SCHEME)
}

/// Percent-encode a single URI path component (RFC 3986 unreserved set kept
/// verbatim, everything else `%XX`-escaped).
///
/// Lives here alongside the artifact-ref schemes because the runner/Lab/daemon
/// execution surface and `core::artifact_ref` both build and parse these
/// references and must agree on the encoding — pushing it to the primitives
/// layer lets both depend on it without a `core` cycle.
pub fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Lenient percent-decode for display: malformed escapes are passed through
/// and invalid UTF-8 is replaced (`from_utf8_lossy`).
pub fn decode_uri_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    decoded.push(byte);
                    index += 3;
                    continue;
                }
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

/// Strict counterpart for externally supplied selectors. Unlike the lenient
/// display decoder, malformed percent escapes and invalid UTF-8 are rejected.
pub fn decode_uri_component_strict(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
        decoded.push(u8::from_str_radix(hex, 16).ok()?);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_runner_artifact_refs() {
        assert!(is_runner_artifact_ref("runner-artifact://r/run/a"));
        assert!(!is_runner_artifact_ref("metadata-only:label"));
        assert!(!is_runner_artifact_ref("/abs/path"));
    }

    #[test]
    fn recognizes_metadata_only_refs() {
        assert!(is_metadata_only_ref("metadata-only:label"));
        assert!(!is_metadata_only_ref("runner-artifact://r/run/a"));
        assert!(!is_metadata_only_ref("/abs/path"));
    }

    #[test]
    fn uri_component_round_trips_reserved_bytes() {
        assert_eq!(encode_uri_component("runner/a"), "runner%2Fa");
        assert_eq!(encode_uri_component("run b"), "run%20b");
        assert_eq!(encode_uri_component("keep-._~"), "keep-._~");
        assert_eq!(decode_uri_component("runner%2Fa"), "runner/a");
        assert_eq!(decode_uri_component("run%20b"), "run b");
    }

    #[test]
    fn strict_decode_rejects_malformed_escapes() {
        assert_eq!(
            decode_uri_component_strict("runner%2Fa").as_deref(),
            Some("runner/a")
        );
        assert_eq!(decode_uri_component_strict("bad%"), None);
        assert_eq!(decode_uri_component_strict("bad%2"), None);
        assert_eq!(decode_uri_component_strict("bad%ZZ"), None);
    }
}
