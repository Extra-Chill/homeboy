use std::time::{Duration, Instant};

use homeboy_core::extension::external_check_detail_api::{
    ExternalCheckDetailHydrationContext, ExternalCheckDetailResolverApi,
};
use homeboy_extension_contract::api::v1::{
    ExtensionApiExternalCheckDetailDiagnosticKind, ExtensionApiExternalCheckDetailHydrateRequest,
    ExtensionApiExternalCheckDetailInventoryRequest,
    ExtensionApiExternalCheckDetailInventoryResponse,
    EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA,
    EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_REQUEST_SCHEMA, EXTENSION_API_V1,
};
use serde::Serialize;

pub(super) const MAX_RESOLVERS: usize = 8;
pub(super) const TOTAL_BUDGET: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolverDiagnostic {
    pub provider: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HydratedDetail {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    pub summary: String,
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_refs: Vec<String>,
}

pub(super) struct ResolverSession {
    api: ExternalCheckDetailResolverApi,
    inventory: ExtensionApiExternalCheckDetailInventoryResponse,
}

impl ResolverSession {
    pub(super) fn discover() -> Self {
        let api = ExternalCheckDetailResolverApi::discover(
            &ExtensionApiExternalCheckDetailInventoryRequest {
                schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
            },
        );
        let inventory = api.inventory_api();
        Self { api, inventory }
    }

    pub(super) fn has_unique_provider(&self, provider: &str) -> bool {
        self.inventory
            .providers
            .iter()
            .filter(|entry| entry.provider.as_deref() == Some(provider))
            .take(2)
            .count()
            == 1
    }

    pub(super) fn hydrate(
        &self,
        provider: &str,
        status: &str,
        target_url: Option<&str>,
        deadline: Instant,
    ) -> (Vec<HydratedDetail>, Vec<ResolverDiagnostic>) {
        let resolve_environment = |name: &str| std::env::var(name).ok();
        let response = self.api.hydrate_api(
            &ExtensionApiExternalCheckDetailHydrateRequest {
                schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                provider: provider.to_string(),
                status: status.to_string(),
                target_url: target_url.map(str::to_string),
            },
            ExternalCheckDetailHydrationContext {
                deadline,
                resolve_environment: &resolve_environment,
            },
        );
        let details = response
            .detail
            .map(|detail| {
                vec![HydratedDetail {
                    provider: detail.provider,
                    build_id: detail.build_id,
                    summary: detail.summary.unwrap_or_default(),
                    actions: detail.actions,
                    artifact_refs: detail.artifact_refs,
                    log_refs: detail.log_refs,
                }]
            })
            .unwrap_or_default();
        let diagnostic = response
            .diagnostic
            .map(|diagnostic| ResolverDiagnostic {
                provider: diagnostic.provider,
                kind: diagnostic_kind(diagnostic.kind).to_string(),
                message: diagnostic.message,
            })
            .or_else(|| {
                response.failure.map(|failure| ResolverDiagnostic {
                    provider: provider.to_string(),
                    kind: "unavailable".to_string(),
                    message: failure.message,
                })
            });
        (details, diagnostic.into_iter().collect())
    }
}

pub(super) fn skipped_for_budget(provider: &str) -> ResolverDiagnostic {
    ResolverDiagnostic {
        provider: provider.to_string(),
        kind: "budget_exhausted".to_string(),
        message: "Resolver invocation limit reached; original check evidence was retained."
            .to_string(),
    }
}

pub(super) fn normalize_target_url(value: &str) -> String {
    homeboy_core::extension::external_check_detail_api::normalize_target_url(value)
}

fn diagnostic_kind(kind: ExtensionApiExternalCheckDetailDiagnosticKind) -> &'static str {
    match kind {
        ExtensionApiExternalCheckDetailDiagnosticKind::Unknown => "unknown",
        ExtensionApiExternalCheckDetailDiagnosticKind::Ambiguous => "ambiguous",
        ExtensionApiExternalCheckDetailDiagnosticKind::Malformed => "malformed",
        ExtensionApiExternalCheckDetailDiagnosticKind::MalformedIdentity => "malformed_identity",
        ExtensionApiExternalCheckDetailDiagnosticKind::Unavailable => "unavailable",
        ExtensionApiExternalCheckDetailDiagnosticKind::BudgetExhausted => "budget_exhausted",
    }
}

#[cfg(feature = "test-support")]
pub(super) fn test_cross_platform_fixture() {
    const FIXTURE_MODE_ENV: &str = "HOMEBOY_EXTERNAL_CHECK_FIXTURE_MODE";

    for (mode, expected_detail, expected_diagnostic, budget, expected_message) in [
        ("success", true, None, Duration::from_secs(10), None),
        (
            "unavailable",
            false,
            Some("unavailable"),
            Duration::from_secs(10),
            None,
        ),
        (
            "malformed",
            false,
            Some("malformed"),
            Duration::from_secs(10),
            None,
        ),
        (
            "missing-executable",
            false,
            Some("unavailable"),
            Duration::from_secs(10),
            None,
        ),
        (
            "timeout",
            false,
            Some("unavailable"),
            Duration::from_millis(200),
            Some("capture files were snapshotted"),
        ),
    ] {
        homeboy::core::test_support::with_isolated_home(|_| {
            install_fixture_extension(mode, FIXTURE_MODE_ENV);
            let session = ResolverSession::discover();
            let started = Instant::now();
            let (details, diagnostics) = session.hydrate(
                "fixture-ci",
                "failure",
                Some("https://example.test/build/42"),
                started + budget,
            );
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(
                details.len() == 1,
                expected_detail,
                "{mode}: {diagnostics:?}"
            );
            assert_eq!(
                diagnostics
                    .first()
                    .map(|diagnostic| diagnostic.kind.as_str()),
                expected_diagnostic,
                "{mode}"
            );
            if let Some(expected_message) = expected_message {
                assert!(diagnostics[0].message.contains(expected_message));
            }
            if mode == "success" {
                assert_eq!(details[0].actions, ["fixture-ci replay 42"]);
            }
        });
    }
}

#[cfg(feature = "test-support")]
fn install_fixture_extension(mode: &str, fixture_mode_env: &str) {
    let root = homeboy::core::paths::homeboy().unwrap();
    let extension = root.join("extensions").join("fixture-external-check");
    std::fs::create_dir_all(&extension).unwrap();
    let command = if mode == "timeout" {
        timeout_fixture_command(&extension)
    } else if mode == "missing-executable" {
        vec!["not-installed-resolver".to_string()]
    } else {
        vec![fixture_program(&extension, fixture_mode_env)]
    };
    std::fs::write(
        extension.join("fixture-external-check.json"),
        serde_json::json!({
            "name": "Fixture external check",
            "version": "1.0.0",
            "external_check_detail_resolvers": [{
                "schema": "homeboy/external-check-detail-resolver/v1",
                "provider": "fixture-ci",
                "command": command,
                "public_env": [fixture_mode_env]
            }]
        })
        .to_string(),
    )
    .unwrap();
    std::env::set_var(fixture_mode_env, mode);
}

#[cfg(feature = "test-support")]
fn timeout_fixture_command(extension: &std::path::Path) -> Vec<String> {
    let name = if cfg!(windows) {
        "fixture-resolver.exe"
    } else {
        "fixture-resolver"
    };
    std::fs::copy(std::env::current_exe().unwrap(), extension.join(name)).unwrap();
    vec![
        name.to_string(),
        "--ignored".to_string(),
        "--exact".to_string(),
        "resolver_fixture_hangs_until_killed".to_string(),
    ]
}

#[cfg(all(feature = "test-support", unix))]
fn fixture_program(extension: &std::path::Path, fixture_mode_env: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    let name = "fixture-resolver";
    let path = extension.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"${fixture_mode_env}\" in\nsuccess) printf '%s\\n' '{{\"schema\":\"homeboy/external-check-detail-response/v1\",\"provider\":\"fixture-ci\",\"summary\":\"fixture hydrated failure\",\"actions\":[\"fixture-ci replay 42\"]}}' ;;\nmalformed) printf '%s' 'not json' ;;\nunavailable) exit 23 ;;\ntimeout) sleep 30 ;;\n*) exit 24 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    name.to_string()
}

#[cfg(all(feature = "test-support", windows))]
fn fixture_program(extension: &std::path::Path, fixture_mode_env: &str) -> String {
    let name = "fixture-resolver.cmd";
    let path = extension.join(name);
    std::fs::write(
        &path,
        format!(
            "@echo off\r\nif \"%{fixture_mode_env}%\"==\"success\" (echo {{\"schema\":\"homeboy/external-check-detail-response/v1\",\"provider\":\"fixture-ci\",\"summary\":\"fixture hydrated failure\",\"actions\":[\"fixture-ci replay 42\"]}}& exit /b 0)\r\nif \"%{fixture_mode_env}%\"==\"malformed\" (set /p =not json<nul& exit /b 0)\r\nif \"%{fixture_mode_env}%\"==\"unavailable\" exit /b 23\r\n:wait_forever\r\ngoto wait_forever\r\n"
        ),
    )
    .unwrap();
    name.to_string()
}

#[cfg(all(feature = "test-support", target_os = "linux"))]
pub(super) fn test_inherited_pipe_holder_cleanup() {
    homeboy_core::extension::external_check_detail_api::test_inherited_pipe_holder_cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_url_normalization_removes_credentials_query_and_fragment() {
        assert_eq!(
            normalize_target_url("https://token@example.test/build/42?secret=yes#fragment"),
            "https://example.test/build/42"
        );
    }

    #[test]
    fn missing_provider_is_diagnostic_only() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let session = ResolverSession::discover();
            let (details, diagnostics) = session.hydrate(
                "missing",
                "failure",
                None,
                Instant::now() + Duration::from_secs(1),
            );
            assert!(details.is_empty());
            assert_eq!(diagnostics[0].kind, "unknown");
        });
    }
}
