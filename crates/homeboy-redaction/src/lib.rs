use regex::{Captures, Regex};
use serde_json::{Map, Value};

const DEFAULT_REPLACEMENT: &str = "[REDACTED]";

/// Trailing key segments that describe *how* a credential is carried rather
/// than naming a different thing. `session_id`, `token_value`, and
/// `auth_header` are still credential keys; `proxy_auth_smoke` is not.
const GENERIC_KEY_QUALIFIERS: &[&str] = &[
    "b64", "base64", "data", "env", "hash", "header", "id", "ids", "plain", "raw", "str", "string",
    "val", "value", "var",
];

/// Longest run of letters still treated as an ordinary English word when it
/// follows a bare `bearer`/`basic`/`token` in free-form log text. Real bearer
/// credentials and Basic base64 blobs are longer than this and are not pure
/// letters.
const PROSE_WORD_MAX_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPolicy {
    sensitive_keys: Vec<String>,
    sensitive_headers: Vec<String>,
    replacement: String,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        let mut policy = Self {
            sensitive_keys: [
                "api_key",
                "apikey",
                "auth",
                "authorization",
                "bearer",
                "client_secret",
                "cookie",
                "credential",
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
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            replacement: DEFAULT_REPLACEMENT.to_string(),
        };
        if let Ok(raw) = std::env::var("HOMEBOY_REDACTION_SENSITIVE_HEADERS") {
            for header in raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                policy = policy.with_sensitive_header(header);
            }
        }
        policy
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

    pub fn sensitive_keys(&self) -> &[String] {
        &self.sensitive_keys
    }

    pub fn sensitive_headers(&self) -> &[String] {
        &self.sensitive_headers
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

    /// Stricter sibling of [`RedactionPolicy::is_sensitive_key`] for keys that
    /// were *inferred from free-form text* rather than read out of a real
    /// key position.
    ///
    /// `is_sensitive_key` matches any key that merely **contains** a sensitive
    /// token. That is the right fail-closed rule for structured positions —
    /// JSON object keys, HTTP header names, query parameter names, env var
    /// names, CLI flags — where the key genuinely names a value and a false
    /// positive costs nothing but a redacted field.
    ///
    /// It is the wrong rule for prose. In console output, any word that
    /// happens to contain `auth`, `key`, `sid`, or `session` becomes a
    /// "sensitive key", and the token after the next `:` or `=` is destroyed.
    /// That is how `=== http-client-proxy-auth-smoke: 3 FAIL of 15 ===` lost
    /// its failure count: `http_client_proxy_auth_smoke` contains `auth`.
    ///
    /// Credential keys name the credential with their *head noun*, so this
    /// predicate requires the sensitive token to be the final key segment
    /// (optionally followed by one generic carrier qualifier). Structured
    /// callers keep the broad rule; only inferred keys are held to this one.
    pub fn is_credential_key_name(&self, key: &str) -> bool {
        let segments = key_segments(key);
        if segments.is_empty() {
            return false;
        }
        if self.matches_credential_suffix(&segments) {
            return true;
        }
        if segments.len() > 1
            && GENERIC_KEY_QUALIFIERS.contains(&segments[segments.len() - 1].as_str())
        {
            return self.matches_credential_suffix(&segments[..segments.len() - 1]);
        }
        false
    }

    fn matches_credential_suffix(&self, segments: &[String]) -> bool {
        if segments.is_empty() {
            return false;
        }
        let joined = segments.join("_");
        self.sensitive_keys
            .iter()
            .chain(self.sensitive_headers.iter())
            .any(|sensitive| {
                let sensitive = normalize_key(sensitive);
                if sensitive.is_empty() {
                    return false;
                }
                joined == sensitive || joined.ends_with(&format!("_{sensitive}"))
            })
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
        let mut redacted = format!("{}?{query}", self.redact_string(base));
        if let Some(fragment) = fragment {
            redacted.push('#');
            redacted.push_str(fragment);
        }
        redacted
    }

    pub fn redact_json(&self, value: &Value) -> Value {
        self.redact_json_with_key(None, value)
    }

    pub fn redact_argv(&self, argv: &[String]) -> Vec<String> {
        redact_argv_with_policy(argv, self)
    }

    /// Redact a single environment-variable (or inline argument) value.
    ///
    /// URL-shaped values get query-aware redaction so non-sensitive path and
    /// query parts survive; everything else goes through inline-assignment
    /// redaction. This is the canonical env-value heuristic shared by the
    /// secret-env plan and argv redaction so the URL-vs-string dispatch lives in
    /// exactly one place.
    pub fn redact_env_value(&self, value: &str) -> String {
        if looks_like_url(value) {
            self.redact_url(value)
        } else {
            self.redact_string(value)
        }
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

pub fn redact_argv(argv: &[String]) -> Vec<String> {
    RedactionPolicy::default().redact_argv(argv)
}

pub fn redact_argv_display(argv: &[String]) -> String {
    redact_argv(argv).join(" ")
}

/// Render redacted argv as a command that is safe to copy into a POSIX shell.
pub fn redact_argv_shell_display(argv: &[String]) -> String {
    homeboy_engine_primitives::shell::quote_args(&redact_argv(argv))
}

fn redact_argv_with_policy(argv: &[String], policy: &RedactionPolicy) -> Vec<String> {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut redact_next_for: Option<String> = None;

    for arg in argv {
        if let Some(flag) = redact_next_for.take() {
            redacted.push(redact_split_flag_value(&flag, arg, policy));
            continue;
        }

        if sensitive_whole_value_flag(arg) || sensitive_pair_value_flag(arg) {
            redacted.push(arg.clone());
            redact_next_for = Some(arg.clone());
            continue;
        }

        if let Some((flag, value)) = arg.split_once('=') {
            if sensitive_whole_value_flag(flag) {
                redacted.push(format!("{flag}={}", policy.replacement()));
                continue;
            }
            if sensitive_pair_value_flag(flag) {
                redacted.push(format!("{flag}={}", redact_key_value_arg(value, policy)));
                continue;
            }
        }

        redacted.push(redact_sensitive_inline_arg(arg, policy));
    }

    redacted
}

fn redact_split_flag_value(flag: &str, value: &str, policy: &RedactionPolicy) -> String {
    if sensitive_whole_value_flag(flag) {
        policy.replacement().to_string()
    } else {
        redact_key_value_arg(value, policy)
    }
}

fn sensitive_whole_value_flag(flag: &str) -> bool {
    matches!(
        normalize_flag(flag).as_str(),
        "secret_env"
            | "provider_auth"
            | "provider_auth_json"
            | "provider_auth_token"
            | "provider_access_token"
            | "provider_refresh_token"
            | "access_token"
            | "refresh_token"
            | "api_key"
            | "attempt_plan"
            | "password"
            | "token"
    )
}

fn sensitive_pair_value_flag(flag: &str) -> bool {
    matches!(normalize_flag(flag).as_str(), "setting" | "setting_json")
}

fn normalize_flag(flag: &str) -> String {
    normalize_key(flag.trim_start_matches('-'))
}

fn redact_key_value_arg(value: &str, policy: &RedactionPolicy) -> String {
    let Some((key, raw_value)) = value.split_once('=') else {
        if let Ok(json) = serde_json::from_str::<Value>(value) {
            return policy.redact_json(&json).to_string();
        }
        return redact_sensitive_inline_arg(value, policy);
    };
    if policy.is_sensitive_key(key) || policy.is_sensitive_header(key) {
        return format!("{key}={}", policy.replacement());
    }
    if let Ok(json) = serde_json::from_str::<Value>(raw_value) {
        return format!("{key}={}", policy.redact_json(&json));
    }
    format!("{key}={}", redact_sensitive_inline_arg(raw_value, policy))
}

fn redact_sensitive_inline_arg(value: &str, policy: &RedactionPolicy) -> String {
    policy.redact_env_value(value)
}

fn normalize_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('-', "_")
}

fn redact_authorization_schemes(value: &str, replacement: &str) -> String {
    // An explicit authorization key is unambiguous provenance: whatever
    // follows it is a credential no matter what it looks like. Redact it
    // unconditionally, preserving any scheme name so the diagnostic still
    // says *how* the request authenticated.
    //
    // This also closes a gap in the previous implementation, which only
    // redacted after a recognized scheme word: `Authorization: opaquevalue`
    // (no scheme) survived both this pass and the inline-assignment pass,
    // because the latter deliberately skips `authorization` keys.
    let header = Regex::new(
        r"(?i)\b((?:proxy-)?authorization)(\s*[:=]\s*)((?:bearer|basic|token|digest|negotiate)\s+)?([^\s,;]+)",
    )
    .expect("authorization header redaction regex is valid");
    let value = header
        .replace_all(value, |captures: &Captures<'_>| {
            format!(
                "{}{}{}{replacement}",
                &captures[1],
                &captures[2],
                captures.get(3).map_or("", |scheme| scheme.as_str())
            )
        })
        .into_owned();
    // A bare scheme word with no authorization key around it is a shape
    // guess, and `bearer`/`basic`/`token` are also ordinary English words.
    // "PASS: basic auth creates Authorization header" is a test name, not a
    // credential; redacting the word after `basic` corrupted it. Only redact
    // when the following token is not plain prose. Credentials reachable this
    // way (JWTs, base64, `sk-`/`ghp_` tokens, hex) all fail the prose test.
    let pattern = Regex::new(r"(?i)\b(bearer|basic|token)\s+([^\s,;]+)")
        .expect("authorization redaction regex is valid");
    let value = pattern
        .replace_all(&value, |captures: &Captures<'_>| {
            if looks_like_prose_word(&captures[2]) {
                captures[0].to_string()
            } else {
                format!("{} {replacement}", &captures[1])
            }
        })
        .into_owned();
    let credentials = Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s:@]+:[^@/\s]+@")
        .expect("URL credential redaction regex is valid");
    credentials
        .replace_all(&value, |captures: &Captures<'_>| {
            format!("{}{}@", &captures[1], replacement)
        })
        .into_owned()
}

fn redact_inline_assignments(value: &str, policy: &RedactionPolicy) -> String {
    let pattern = Regex::new(r"([A-Za-z0-9_.-]+)(\s*[:=]\s*)([^&\s,;]+)")
        .expect("inline secret redaction regex is valid");
    pattern
        .replace_all(value, |captures: &Captures<'_>| {
            let key = &captures[1];
            // Authorization keys were already handled, scheme name intact.
            if is_authorization_key(key) {
                return captures[0].to_string();
            }
            // The "key" here was inferred from arbitrary text, so it is held
            // to the strict credential-key rule instead of the broad
            // substring rule used for real key positions.
            if policy.is_credential_key_name(key) {
                format!("{}{}{}", key, &captures[2], policy.replacement)
            } else {
                captures[0].to_string()
            }
        })
        .into_owned()
}

fn is_authorization_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    normalized == "authorization" || normalized.ends_with("_authorization")
}

/// Split a key into lowercase segments, treating `_`, `-`, `.`, `/`, spaces,
/// and camelCase transitions as boundaries. `X-Api-Key` and `apiKey` both
/// become `["api", "key"]`.
fn key_segments(key: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut previous_is_lower_or_digit = false;
    for character in key.trim().chars() {
        if matches!(character, '_' | '-' | '.' | '/' | ' ' | ':') {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            previous_is_lower_or_digit = false;
            continue;
        }
        if character.is_ascii_uppercase() && previous_is_lower_or_digit && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
        previous_is_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
        current.push(character.to_ascii_lowercase());
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Whether a token is shaped like an ordinary word rather than a credential.
/// Deliberately conservative: anything with a digit, punctuation, mixed case,
/// or more than [`PROSE_WORD_MAX_LEN`] letters is treated as a credential.
fn looks_like_prose_word(value: &str) -> bool {
    if value.is_empty() || value.len() > PROSE_WORD_MAX_LEN {
        return false;
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphabetic())
    {
        return false;
    }
    let all_lowercase = value
        .chars()
        .all(|character| character.is_ascii_lowercase());
    let all_uppercase = value
        .chars()
        .all(|character| character.is_ascii_uppercase());
    let capitalized = value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_uppercase())
        && value
            .chars()
            .skip(1)
            .all(|character| character.is_ascii_lowercase());
    all_lowercase || capitalized || (all_uppercase && value.len() <= 5)
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
        assert_eq!(
            policy.redact_string("Authorization: ToKeN ghp_secret"),
            "Authorization: ToKeN [REDACTED]"
        );
    }

    #[test]
    fn redacts_connection_string_credentials_in_strings_and_urls() {
        let policy = RedactionPolicy::default();
        let secret = "postgres://app:super-secret@db.example.test/app";

        assert_eq!(
            policy.redact_string(secret),
            "postgres://[REDACTED]@db.example.test/app"
        );
        assert_eq!(
            policy.redact_url("postgres://app:super-secret@db.example.test/app?token=also-secret"),
            "postgres://[REDACTED]@db.example.test/app?token=[REDACTED]"
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
    fn preserves_low_entropy_diagnostic_text_and_failure_counts() {
        // Verbatim console output from the reproduction in #10521. Every line
        // here is test output, not a credential: `http-client-proxy-auth-smoke`
        // merely contains `auth`, and `basic`/`Basic` are English words.
        let policy = RedactionPolicy::default();
        let diagnostics = "[1] Standard auth options\n\
             PASS: basic auth creates Authorization header\n\
             PASS: bearer auth creates Authorization header\n\
             [2] Auth ref options\n\
             FAIL: auth_ref resolves auth options\n\
             FAIL: auth_ref resolves proxy URL\n\
             FAIL: auth_ref resolved Basic header is applied\n\
             === http-client-proxy-auth-smoke: 3 FAIL of 15 ===";

        assert_eq!(policy.redact_string(diagnostics), diagnostics);
    }

    #[test]
    fn inferred_keys_only_redact_when_the_head_noun_is_a_credential() {
        let policy = RedactionPolicy::default();

        for credential_key in [
            "token",
            "api_token",
            "X-Api-Key",
            "apiKey",
            "clientSecret",
            "refresh_token",
            "session_id",
            "auth_header",
            "PASSWORD",
        ] {
            assert!(
                policy.is_credential_key_name(credential_key),
                "{credential_key} must stay redacted"
            );
        }

        for diagnostic_key in [
            "http-client-proxy-auth-smoke",
            "auth_ref",
            "PASS",
            "monkey",
            "keyboard",
            "considered",
        ] {
            assert!(
                !policy.is_credential_key_name(diagnostic_key),
                "{diagnostic_key} is diagnostic text, not a credential key"
            );
        }
    }

    #[test]
    fn structured_key_positions_keep_the_broad_fail_closed_rule() {
        // Narrowing applies only to keys inferred from prose. Real key
        // positions (env names, JSON keys, headers, query params) still match
        // on substring so a novel credential name cannot slip through.
        let policy = RedactionPolicy::default();

        assert!(policy.is_sensitive_key("http-client-proxy-auth-smoke"));
        assert_eq!(
            policy.redact_json(&json!({ "proxy_auth_settings": "value" })),
            json!({ "proxy_auth_settings": "[REDACTED]" })
        );
        assert_eq!(
            policy.redact_url("/path?proxy_auth_settings=value&ok=1"),
            "/path?proxy_auth_settings=[REDACTED]&ok=1"
        );
    }

    #[test]
    fn redacts_authorization_header_values_without_a_scheme_word() {
        let policy = RedactionPolicy::default();

        assert_eq!(
            policy.redact_string("Authorization: opaquecredential"),
            "Authorization: [REDACTED]"
        );
        assert_eq!(
            policy.redact_string("Proxy-Authorization: Basic secretvalue"),
            "Proxy-Authorization: Basic [REDACTED]"
        );
    }

    #[test]
    fn bare_scheme_words_still_redact_credential_shaped_tokens() {
        let policy = RedactionPolicy::default();

        for credential in [
            "abc.def.ghi",
            "dXNlcjpzZWNyZXQ=",
            "ghp_secret",
            "sk-test-value",
            "aBcDeFgHiJkL",
            "0123456789abcdef",
        ] {
            let redacted = policy.redact_string(&format!("sent bearer {credential} upstream"));
            assert_eq!(
                redacted, "sent bearer [REDACTED] upstream",
                "bearer {credential} must stay redacted"
            );
        }
    }

    #[test]
    fn redaction_is_idempotent_over_already_redacted_text() {
        let policy = RedactionPolicy::default();
        let once = policy.redact_string("Authorization: Bearer abc.def.ghi token=secret-value");

        assert_eq!(policy.redact_string(&once), once);
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

    #[test]
    fn redacts_sensitive_argv_split_and_equals_forms() {
        let argv = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--setting".to_string(),
            "api_token=abc123".to_string(),
            "--setting=password=hunter2".to_string(),
            "--setting-json".to_string(),
            r#"provider={"access_token":"token-value","safe":"ok"}"#.to_string(),
            "--setting-json".to_string(),
            r#"{"client_secret":"client-secret","safe":"ok"}"#.to_string(),
            r#"--setting-json={"refresh_token":"refresh-value","safe":"ok"}"#.to_string(),
            "--secret-env".to_string(),
            "OPENAI_API_KEY=sk-test".to_string(),
            "--secret-env=ANTHROPIC_API_KEY=sk-ant".to_string(),
            "--provider-auth-token".to_string(),
            "provider-token".to_string(),
            "--attempt-plan".to_string(),
            r#"{"plan_id":"private-plan"}"#.to_string(),
            "--attempt-plan=@/private/attempt-plan.json".to_string(),
            "--url=https://example.test/?token=query-token&ok=1".to_string(),
        ];

        assert_eq!(
            redact_argv(&argv),
            vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--setting".to_string(),
                "api_token=[REDACTED]".to_string(),
                "--setting=password=[REDACTED]".to_string(),
                "--setting-json".to_string(),
                r#"provider={"access_token":"[REDACTED]","safe":"ok"}"#.to_string(),
                "--setting-json".to_string(),
                r#"{"client_secret":"[REDACTED]","safe":"ok"}"#.to_string(),
                r#"--setting-json={"refresh_token":"[REDACTED]","safe":"ok"}"#.to_string(),
                "--secret-env".to_string(),
                "[REDACTED]".to_string(),
                "--secret-env=[REDACTED]".to_string(),
                "--provider-auth-token".to_string(),
                "[REDACTED]".to_string(),
                "--attempt-plan".to_string(),
                "[REDACTED]".to_string(),
                "--attempt-plan=[REDACTED]".to_string(),
                "--url=https://example.test/?token=[REDACTED]&ok=1".to_string(),
            ]
        );
    }

    #[test]
    fn shell_display_redacts_before_quoting_copyable_argv() {
        let argv = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run".to_string(),
            "#8949".to_string(),
            "path with spaces".to_string(),
            "O'Brien".to_string(),
            "$HOME/work".to_string(),
            "--provider-auth-token".to_string(),
            "secret-value".to_string(),
        ];

        let display = redact_argv_shell_display(&argv);

        assert_eq!(
            display,
            "homeboy agent-task run '#8949' 'path with spaces' 'O'\\''Brien' '$HOME/work' --provider-auth-token '[REDACTED]'"
        );
        assert!(!display.contains("secret-value"));
        assert!(std::process::Command::new("sh")
            .args(["-n", "-c", &display])
            .status()
            .expect("shell is available")
            .success());
    }
}
