//! Provider usage caps: a distinct, temporary provider state carrying a known
//! reset time.
//!
//! A flat-rate provider (`opencode-go`, `zai-coding-plan`, ...) that has hit
//! its rolling usage window is not unhealthy, misconfigured, or lacking
//! credentials — its credentials still resolve, so plain readiness reports it
//! `ready`. Without this module, provider rotation has no way to tell a
//! capped provider apart from a healthy one, so a large fanout keeps
//! re-dispatching to it and burns a provider execution per task just to
//! rediscover the same cap (#13644).
//!
//! This module owns two things:
//! - detecting a usage-cap signature (plus its reset time) in provider output
//!   text, and
//! - an explicit, caller-owned registry recording which provider routes are
//!   presently capped, mirroring `ProviderRuntimeReadinessCache`: a caller
//!   creates one instance per scope (a plan's live dispatch loop, a batch
//!   preflight) and threads it through so siblings share what one task's
//!   failure already taught Homeboy, instead of each spending its own
//!   execution to relearn it.

use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::agent_task::AgentTaskOutcome;
use crate::agent_task::AgentTaskRequest;
use homeboy_engine_primitives::content_hash;

/// Diagnostic class attached to an outcome when Homeboy detects a provider
/// usage cap in its output. Distinct from the generic rate-limit text match
/// so the reset time is legible in run diagnostics rather than buried in a
/// free-text summary.
pub const AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS: &str = "agent_task.provider_usage_cap";

/// The registry key identifying one rotation-selectable provider route. A
/// rotation entry pins exactly this identity (backend + selector), so caps
/// are recorded and consulted at the same granularity rotation dispatches at.
pub fn provider_usage_cap_key(backend: &str, selector: Option<&str>) -> String {
    match selector
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
    {
        Some(selector) => format!("{backend}::{selector}"),
        None => backend.to_string(),
    }
}

/// Model-scoped route key used by scheduler dispatch. Distinct models on one
/// runtime backend can represent independent provider accounts or cap windows.
pub fn provider_usage_cap_key_for_model(
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
) -> String {
    let route = provider_usage_cap_key(backend, selector);
    match model.map(str::trim).filter(|model| !model.is_empty()) {
        Some(model) => format!("{route}::{model}"),
        None => route,
    }
}

/// Stable capacity key for an effective route. Account/provider configuration
/// and runtime selection are part of the identity so independent accounts on
/// one backend/model never inherit each other's cap evidence.
pub fn provider_usage_cap_key_for_request(request: &AgentTaskRequest) -> String {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "backend": request.executor.backend,
        "selector": request.executor.selector,
        "runtime_selection": request.executor.runtime_selection(),
        "required_capabilities": request.executor.required_capabilities,
        "effective_config": provider_capacity_config(request),
        "resolved_runtime_identity": request.metadata.get("resolved_runtime_identity"),
    }))
    .expect("provider route capacity identity serializes");
    content_hash::sha256_hex(&encoded)
}

pub(crate) fn provider_capacity_config(request: &AgentTaskRequest) -> serde_json::Value {
    let mut config =
        super::effective_provider_config(&request.executor.config, request.executor.model())
            .as_object()
            .cloned()
            .unwrap_or_default();
    config.remove("workspace_root");
    if let Some(workspace) = config
        .get_mut("workspace")
        .and_then(serde_json::Value::as_object_mut)
    {
        workspace.remove("root");
        if workspace.is_empty() {
            config.remove("workspace");
        }
    }
    if let Some(runtime_env) = config
        .get_mut("runtime_env")
        .and_then(serde_json::Value::as_object_mut)
    {
        runtime_env.remove("TMPDIR");
        if runtime_env.is_empty() {
            config.remove("runtime_env");
        }
    }
    serde_json::Value::Object(config)
}

/// Providers Homeboy has learned are presently over their usage cap, plus the
/// instant each is expected to reset.
///
/// Explicit and caller-owned rather than global: a plan's live dispatch loop
/// and a batch preflight each create their own instance so unrelated runs
/// cannot leak cap state into each other, matching the existing
/// `ProviderRuntimeReadinessCache` pattern.
#[derive(Debug, Default, Clone)]
pub struct ProviderUsageCapRegistry {
    capped: BTreeMap<String, DateTime<Utc>>,
}

impl ProviderUsageCapRegistry {
    /// Record (or extend) a usage cap for `key`. A later, later-resetting
    /// observation wins; an earlier reset time already recorded is not
    /// clobbered by a stale re-observation.
    pub fn record(&mut self, key: impl Into<String>, reset_at: DateTime<Utc>) {
        let key = key.into();
        match self.capped.get(&key) {
            Some(existing) if *existing >= reset_at => {}
            _ => {
                // Stale rows may be discarded, but active capacity evidence is
                // authoritative and must never be silently dropped merely
                // because many accounts are active at once.
                let now = Utc::now();
                self.capped.retain(|_, reset| *reset > now);
                self.capped.insert(key, reset_at);
            }
        }
    }

    /// The still-active reset time for `key`, or `None` when unrecorded or
    /// the recorded reset time has already passed. A passed cap is treated as
    /// lifted rather than requiring an explicit prune pass.
    pub fn active(&self, key: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.capped
            .get(key)
            .filter(|reset_at| **reset_at > now)
            .copied()
    }

    pub fn is_empty(&self) -> bool {
        self.capped.is_empty()
    }
}

/// Read the reset time Homeboy already attached to an outcome via
/// [`AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS`], if any.
pub fn reset_at_from_outcome(outcome: &AgentTaskOutcome) -> Option<DateTime<Utc>> {
    outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS)
        .and_then(|diagnostic| diagnostic.data.get("reset_at"))
        .and_then(serde_json::Value::as_str)
        .and_then(|reset_at| DateTime::parse_from_rfc3339(reset_at).ok())
        .map(|reset_at| reset_at.with_timezone(&Utc))
}

fn usage_cap_phrase_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)usage[ _-]?(?:limit|cap)").expect("static usage-cap phrase regex")
    })
}

fn relative_reset_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)resets?\s+in\s+(?:(\d+)\s*h(?:ou)?r[s]?)?\s*,?\s*(?:(\d+)\s*min(?:ute)?[s]?)?",
        )
        .expect("static relative reset regex")
    })
}

fn absolute_reset_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)resets?\s+at\s+(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})")
            .expect("static absolute reset regex")
    })
}

/// Detect a usage-cap signature in provider output text and compute its reset
/// time relative to `now`.
///
/// Requires both a usage-limit/usage-cap phrase and a parseable reset time:
/// the phrase alone is not distinct enough evidence to skip a provider route,
/// and a bare reset time without the phrase is not evidence of a cap at all.
///
/// Supports the two reset formats providers were observed emitting (#13644):
/// - relative: "Resets in 3hr 3min."
/// - absolute: "Resets at 2026-08-27 12:37:03." (treated as UTC; providers do
///   not consistently name a timezone in this text)
pub fn detect_usage_cap(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if !usage_cap_phrase_regex().is_match(text) {
        return None;
    }
    if let Some(captures) = absolute_reset_regex().captures(text) {
        let raw = captures.get(1)?.as_str().replace(' ', "T");
        let naive = chrono::NaiveDateTime::parse_from_str(&raw, "%Y-%m-%dT%H:%M:%S").ok()?;
        return Some(Utc.from_utc_datetime(&naive));
    }
    if let Some(captures) = relative_reset_regex().captures(text) {
        let hours: i64 = captures
            .get(1)
            .and_then(|value| value.as_str().parse().ok())
            .unwrap_or(0);
        let minutes: i64 = captures
            .get(2)
            .and_then(|value| value.as_str().parse().ok())
            .unwrap_or(0);
        if hours == 0 && minutes == 0 {
            return None;
        }
        return Some(now + chrono::Duration::hours(hours) + chrono::Duration::minutes(minutes));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 27, 9, 34, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn detects_a_relative_reset_window() {
        let reset_at = detect_usage_cap("5-hour usage limit reached. Resets in 3hr 3min.", now())
            .expect("relative reset parsed");
        assert_eq!(
            reset_at,
            now() + chrono::Duration::hours(3) + chrono::Duration::minutes(3)
        );
    }

    #[test]
    fn detects_an_absolute_reset_timestamp() {
        let reset_at = detect_usage_cap(
            "Usage limit reached for 5 hour. Resets at 2026-08-27 12:37:03.",
            now(),
        )
        .expect("absolute reset parsed");
        assert_eq!(
            reset_at,
            Utc.with_ymd_and_hms(2026, 8, 27, 12, 37, 3)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn a_bare_rate_limit_without_the_usage_cap_phrase_is_not_a_cap() {
        assert!(detect_usage_cap("429 too many requests, try again shortly", now()).is_none());
    }

    #[test]
    fn a_usage_cap_phrase_without_a_parseable_reset_time_is_not_recorded() {
        assert!(detect_usage_cap("daily usage limit reached", now()).is_none());
    }

    #[test]
    fn registry_reports_active_only_before_the_reset_time_passes() {
        let mut registry = ProviderUsageCapRegistry::default();
        let key = provider_usage_cap_key("zai-coding-plan", None);
        let reset_at = now() + chrono::Duration::hours(1);
        registry.record(&key, reset_at);

        assert_eq!(registry.active(&key, now()), Some(reset_at));
        assert_eq!(
            registry.active(&key, reset_at + chrono::Duration::seconds(1)),
            None
        );
        assert_eq!(registry.active("other-backend", now()), None);
    }

    #[test]
    fn registry_key_includes_the_selector_when_present() {
        assert_eq!(provider_usage_cap_key("opencode-go", None), "opencode-go");
        assert_eq!(
            provider_usage_cap_key("opencode-go", Some("secondary")),
            "opencode-go::secondary"
        );
        assert_ne!(
            provider_usage_cap_key_for_model("opencode", None, Some("zai/glm")),
            provider_usage_cap_key_for_model("opencode", None, Some("openai/gpt"))
        );
    }

    #[test]
    fn registry_keeps_the_later_reset_time_and_does_not_regress_on_a_stale_observation() {
        let mut registry = ProviderUsageCapRegistry::default();
        let key = provider_usage_cap_key("opencode-go", None);
        let later = now() + chrono::Duration::hours(2);
        registry.record(&key, later);
        registry.record(&key, now() + chrono::Duration::minutes(5));

        assert_eq!(registry.active(&key, now()), Some(later));
    }

    #[test]
    fn registry_preserves_more_than_sixty_four_active_capacity_bindings() {
        let mut registry = ProviderUsageCapRegistry::default();
        let reset_at = Utc::now() + chrono::Duration::hours(1);
        for index in 0..64 {
            registry.record(format!("active-{index}"), reset_at);
        }

        registry.record("overflow", reset_at);

        assert_eq!(registry.capped.len(), 65);
        for index in 0..64 {
            assert_eq!(
                registry.active(&format!("active-{index}"), Utc::now()),
                Some(reset_at)
            );
        }
        assert_eq!(registry.active("overflow", Utc::now()), Some(reset_at));
    }

    #[test]
    fn bounded_registry_prunes_expired_bindings_before_admitting_new_evidence() {
        let mut registry = ProviderUsageCapRegistry::default();
        registry.capped.insert(
            "expired".to_string(),
            Utc::now() - chrono::Duration::seconds(1),
        );
        for index in 0..63 {
            registry.capped.insert(
                format!("active-{index}"),
                Utc::now() + chrono::Duration::hours(1),
            );
        }
        registry.record("replacement", Utc::now() + chrono::Duration::hours(1));

        assert!(!registry.capped.contains_key("expired"));
        assert!(registry.capped.contains_key("replacement"));
    }
}
