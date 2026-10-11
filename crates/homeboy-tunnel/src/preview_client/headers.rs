use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::{self, SeqAccess, Visitor};
use serde::Deserializer;
use std::collections::BTreeMap;

use super::types::PreviewClientForwardError;

pub(super) fn local_request_url(
    local_origin: &str,
    path: &str,
) -> std::result::Result<String, PreviewClientForwardError> {
    if !path.starts_with('/') {
        return Err(PreviewClientForwardError {
            kind: "invalid_path".to_string(),
            message: "preview ingress request path must start with /".to_string(),
        });
    }
    Ok(format!("{}{}", local_origin.trim_end_matches('/'), path))
}

pub(super) fn decode_body(
    body_base64: Option<&str>,
) -> std::result::Result<Option<Vec<u8>>, PreviewClientForwardError> {
    use base64::Engine;
    body_base64
        .map(|body| {
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(|err| PreviewClientForwardError {
                    kind: "invalid_body".to_string(),
                    message: format!("preview ingress request body is not valid base64: {err}"),
                })
        })
        .transpose()
}

pub(super) fn forward_request_headers(headers: &BTreeMap<String, String>) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers {
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "connection" | "host" | "content-length" | "transfer-encoding" | "upgrade"
        ) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            forwarded.insert(name, value);
        }
    }
    forwarded
}

pub(super) fn response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let normalized = name.as_str().to_ascii_lowercase();
            if matches!(
                normalized.as_str(),
                "connection" | "transfer-encoding" | "upgrade"
            ) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|value| (normalized, value.to_string()))
        })
        .collect()
}

pub(super) fn cors_headers(
    mut headers: Vec<(String, String)>,
    path: &str,
) -> Vec<(String, String)> {
    push_header_if_missing(&mut headers, "access-control-allow-origin", "*");
    push_header_if_missing(
        &mut headers,
        "access-control-allow-methods",
        "GET, HEAD, OPTIONS",
    );
    push_header_if_missing(&mut headers, "access-control-allow-headers", "*");
    if path.split('?').next().unwrap_or(path).ends_with(".json") {
        push_header_if_missing(&mut headers, "content-type", "application/json");
    }
    headers
}

fn push_header_if_missing(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if !headers
        .iter()
        .any(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
    {
        headers.push((name.to_string(), value.to_string()));
    }
}

pub(super) fn deserialize_response_headers<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<(String, String)>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ResponseHeadersVisitor;

    impl<'de> Visitor<'de> for ResponseHeadersVisitor {
        type Value = Vec<(String, String)>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a header object or ordered [name, value] header pairs")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut headers = Vec::new();
            while let Some((name, value)) = map.next_entry::<String, String>()? {
                headers.push((name, value));
            }
            Ok(headers)
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut headers = Vec::new();
            while let Some((name, value)) = seq.next_element::<(String, String)>()? {
                headers.push((name, value));
            }
            Ok(headers)
        }
    }

    deserializer.deserialize_any(ResponseHeadersVisitor)
}
