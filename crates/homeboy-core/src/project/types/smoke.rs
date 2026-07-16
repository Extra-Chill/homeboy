use serde::{Deserialize, Serialize};

/// Post-deploy smoke check configuration.
///
/// Opt-in, config-driven front-end health check that runs after a successful
/// real deploy. It fetches a configured URL as a fresh (cookie-less) visitor
/// and asserts the HTTP status (and optionally a content substring), failing
/// the deploy when the smoke fails.
///
/// This is deliberately generic: core only knows "fetch a URL, assert a
/// status/content". The concrete front-end URL (e.g. a site home page)
/// belongs in the project config, not in core, so the smoke step stays
/// stack-agnostic. It exists to catch runtime-fataling releases that pass
/// syntax-only checks and never get exercised by a real page load
/// — see homeboy#5471.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmokeCheckConfig {
    /// Whether the post-deploy smoke check runs. Defaults to false (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// URL to fetch after deploy. Required when `enabled` is true.
    #[serde(default)]
    pub url: String,
    /// HTTP status code that counts as healthy. Defaults to 200.
    #[serde(default = "default_smoke_expected_status")]
    pub expected_status: u16,
    /// Optional substring that must appear in the response body. When set, the
    /// smoke also fetches the body and fails if the substring is absent — a
    /// cheap way to assert "real page rendered", not just "server answered".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_content: Option<String>,
    /// Request timeout in seconds. Defaults to 15.
    #[serde(default = "default_smoke_timeout_secs")]
    pub timeout_secs: u64,
    /// When true, a failed smoke check only warns instead of failing the deploy.
    /// Defaults to false: a failing smoke fails the deploy so runtime-fataling
    /// releases are flagged for rollback rather than left live.
    #[serde(default)]
    pub warn_only: bool,
}

fn default_smoke_expected_status() -> u16 {
    200
}

fn default_smoke_timeout_secs() -> u64 {
    15
}

impl Default for SmokeCheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            expected_status: default_smoke_expected_status(),
            expect_content: None,
            timeout_secs: default_smoke_timeout_secs(),
            warn_only: false,
        }
    }
}
