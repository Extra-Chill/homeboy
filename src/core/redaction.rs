use regex::{Captures, Regex};
use serde_json::{Map, Value};

const DEFAULT_REPLACEMENT: &str = "[REDACTED]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPolicy {
    sensitive_keys: Vec<String>,
    sensitive_headers: Vec<String>,
    replacement: String,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            sensitive_keys: [
                "api_key",
                "apikey",
                "auth",
                "authorization",
                "bearer",
                "client_secret",
                "cookie",
                "key",
                "nonce",
                "passwd",
                "password",
                "refresh_token",
                "secret",
                "session",
                "sid",
                "token",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            sensitive_headers: [
                "authorization",
                "cookie",
                "proxy-authorization",
                "set-cookie",
                "x-api-key",
                "x-auth-token",
                "x-csrf-token",
                "x-wp-nonce",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            replacement: DEFAULT_REPLACEMENT.to_string(),
        }
    }
}

impl RedactionPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_replacement(mut self, replacement: impl Into<String>) -> Self {
        self.replacement = replacement.into();
        self
    }

    pub fn with_sensitive_key(mut self, key: impl Into<String>) -> Self {
        self.sensitive_keys.push(normalize_key(&key.into()));
        self
    }

    pub fn with_sensitive_header(mut self, header: impl Into<String>) -> Self {
        self.sensitive_headers.push(normalize_key(&header.into()));
        self
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    pub fn is_sensitive_key(&self, key: &str) -> bool {
        let key = normalize_key(key);
        self.sensitive_keys
            .iter()
            .any(|sensitive| key == *sensitive || key.contains(sensitive))
    }

    pub fn is_sensitive_header(&self, header: &str) -> bool {
        let header = normalize_key(header);
        self.sensitive_headers
            .iter()
            .any(|sensitive| header == *sensitive || header.contains(sensitive))
    }

    pub fn redact_string(&self, value: &str) -> String {
        let value = redact_authorization_schemes(value, &self.replacement);
        redact_inline_assignments(&value, self)
    }

    pub fn redact_url(&self, value: &str) -> String {
        let (without_fragment, fragment) = split_once(value, '#');
        let (base, query) = split_once(without_fragment, '?');
        let Some(query) = query else {
            return self.redact_string(value);
        };

        let query = query
            .split('&')
            .map(|part| self.redact_query_part(part))
            .collect::<Vec<_>>()
            .join("&");
        let mut redacted = format!("{base}?{query}");
        if let Some(fragment) = fragment {
            redacted.push('#');
            redacted.push_str(fragment);
        }
        redacted
    }

    pub fn redact_json(&self, value: &Value) -> Value {
        self.redact_json_with_key(None, value)
    }

    fn redact_json_with_key(&self, key: Option<&str>, value: &Value) -> Value {
        if key.is_some_and(|key| self.is_sensitive_key(key) || self.is_sensitive_header(key)) {
            return Value::String(self.replacement.clone());
        }

        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), self.redact_json_with_key(Some(key), value)))
                    .collect::<Map<_, _>>(),
            ),
            Value::Array(items) => {
                Value::Array(items.iter().map(|value| self.redact_json(value)).collect())
            }
            Value::String(value) => {
                if looks_like_url(value) {
                    Value::String(self.redact_url(value))
                } else {
                    Value::String(self.redact_string(value))
                }
            }
            _ => value.clone(),
        }
    }

    fn redact_query_part(&self, part: &str) -> String {
        let Some((key, _value)) = part.split_once('=') else {
            return if self.is_sensitive_key(part) {
                format!("{part}={}", self.replacement)
            } else {
                part.to_string()
            };
        };
        if self.is_sensitive_key(key) {
            format!("{key}={}", self.replacement)
        } else {
            part.to_string()
        }
    }
}

pub fn redact_string(value: &str) -> String {
    RedactionPolicy::default().redact_string(value)
}

pub fn redact_url(value: &str) -> String {
    RedactionPolicy::default().redact_url(value)
}

pub fn redact_json(value: &Value) -> Value {
    RedactionPolicy::default().redact_json(value)
}

fn normalize_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('-', "_")
}

fn redact_authorization_schemes(value: &str, replacement: &str) -> String {
    let pattern = Regex::new(r"(?i)\b(bearer|basic)\s+[^\s,;]+")
        .expect("authorization redaction regex is valid");
    pattern
        .replace_all(value, |captures: &Captures<'_>| {
            format!("{} {replacement}", &captures[1])
        })
        .into_owned()
}

fn redact_inline_assignments(value: &str, policy: &RedactionPolicy) -> String {
    let pattern = Regex::new(r"([A-Za-z0-9_.-]+)(\s*[:=]\s*)([^&\s,;]+)")
        .expect("inline secret redaction regex is valid");
    pattern
        .replace_all(value, |captures: &Captures<'_>| {
            let key = &captures[1];
            if normalize_key(key) == "authorization" {
                return captures[0].to_string();
            }
            if policy.is_sensitive_key(key) || policy.is_sensitive_header(key) {
                format!("{}{}{}", key, &captures[2], policy.replacement)
            } else {
                captures[0].to_string()
            }
        })
        .into_owned()
}

fn split_once(value: &str, delimiter: char) -> (&str, Option<&str>) {
    match value.split_once(delimiter) {
        Some((left, right)) => (left, Some(right)),
        None => (value, None),
    }
}

fn looks_like_url(value: &str) -> bool {
    value.contains("://") || value.starts_with('/') && value.contains('?')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_authorization_schemes_in_strings() {
        let policy = RedactionPolicy::default();

        assert_eq!(
            policy.redact_string("Authorization: Bearer abc.def.ghi"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            policy.redact_string("proxy Basic dXNlcjpzZWNyZXQ="),
            "proxy Basic [REDACTED]"
        );
    }

    #[test]
    fn redacts_inline_secret_assignments() {
        let policy = RedactionPolicy::default();

        assert_eq!(
            policy.redact_string("token=abc password: hunter2 safe=value"),
            "token=[REDACTED] password: [REDACTED] safe=value"
        );
    }

    #[test]
    fn redacts_sensitive_url_query_values_deterministically() {
        let policy = RedactionPolicy::default();

        assert_eq!(
            policy.redact_url("https://example.test/path?b=2&token=abc&nonce=xyz#frag"),
            "https://example.test/path?b=2&token=[REDACTED]&nonce=[REDACTED]#frag"
        );
    }

    #[test]
    fn redacts_json_values_with_key_context() {
        let policy = RedactionPolicy::default();
        let value = json!({
            "headers": {
                "Authorization": "Bearer abc",
                "Accept": "application/json"
            },
            "url": "https://example.test/?access_token=abc&ok=1",
            "nested": [{ "clientSecret": "value" }]
        });

        assert_eq!(
            policy.redact_json(&value),
            json!({
                "headers": {
                    "Authorization": "[REDACTED]",
                    "Accept": "application/json"
                },
                "url": "https://example.test/?access_token=[REDACTED]&ok=1",
                "nested": [{ "clientSecret": "[REDACTED]" }]
            })
        );
    }

    #[test]
    fn supports_custom_keys_headers_and_replacement() {
        let policy = RedactionPolicy::new()
            .with_sensitive_key("tenant")
            .with_sensitive_header("x-private")
            .with_replacement("***");

        assert_eq!(
            policy.redact_url("/path?tenant=123&ok=1"),
            "/path?tenant=***&ok=1"
        );
        assert_eq!(
            policy.redact_json(&json!({ "x-private": "secret" })),
            json!({ "x-private": "***" })
        );
    }
}
