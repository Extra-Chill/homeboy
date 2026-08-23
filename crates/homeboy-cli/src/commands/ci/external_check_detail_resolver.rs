//! Sandboxed execution of extension-owned external check detail resolvers.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use homeboy::core::process::{force_terminate_process_tree_bounded, ProcessContainment};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::extension::{
    load_all_extensions, ExternalCheckDetailRequest, ExternalCheckDetailResolverConfig,
    ExternalCheckDetailResponse, EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA,
    EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA,
};
use homeboy_engine_primitives::command::{
    terminate_process_tree_and_reap, terminate_remaining_process_group, ControllerChildGuard,
};
use serde::Serialize;

pub(super) const MAX_RESOLVERS: usize = 8;
pub(super) const TOTAL_BUDGET: Duration = Duration::from_secs(20);
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);

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

#[derive(Debug)]
enum InvocationFailure {
    MalformedResponse,
    Unavailable(String),
}

pub(super) fn hydrate(
    provider: &str,
    status: &str,
    target_url: Option<&str>,
    deadline: Instant,
) -> (Vec<HydratedDetail>, Vec<ResolverDiagnostic>) {
    let mut diagnostics = Vec::new();
    let mut resolvers = Vec::new();
    for manifest in load_all_extensions().unwrap_or_default() {
        for resolver in manifest.external_check_detail_resolvers {
            if resolver.provider == provider {
                resolvers.push((
                    manifest.id.clone(),
                    manifest.extension_path.clone(),
                    resolver,
                ));
            }
        }
    }
    if resolvers.len() > 1 {
        return (
            Vec::new(),
            vec![ResolverDiagnostic {
                provider: provider.into(),
                kind: "ambiguous".into(),
                message:
                    "Multiple extensions declare this exact provider; no resolver was invoked."
                        .into(),
            }],
        );
    }
    let Some((extension, extension_path, resolver)) = resolvers.pop() else {
        return (
            Vec::new(),
            vec![ResolverDiagnostic {
                provider: provider.into(),
                kind: "unknown".into(),
                message: "No installed extension declares this provider. Install or enable the extension that owns this provider, then rerun CI triage.".into(),
            }],
        );
    };
    if let Err(error) = resolver.validate() {
        diagnostics.push(ResolverDiagnostic {
            provider: provider.into(),
            kind: "malformed".into(),
            message: format!("Extension {extension} declares an invalid resolver: {error}"),
        });
        return (Vec::new(), diagnostics);
    }
    let Some(extension_path) = extension_path else {
        return (
            Vec::new(),
            vec![ResolverDiagnostic {
                provider: provider.into(),
                kind: "malformed".into(),
                message: format!("Extension {extension} has no extension path for its resolver."),
            }],
        );
    };
    // Canonicalize before reading declared secrets. A symlinked program must
    // never receive projected credentials from outside its extension root.
    let (extension_path, program) = match resolve_program(Path::new(&extension_path), &resolver) {
        Ok(paths) => paths,
        Err(message) => {
            return (
                Vec::new(),
                vec![ResolverDiagnostic {
                    provider: provider.into(),
                    kind: if message.starts_with("program cannot be resolved") {
                        "unavailable".into()
                    } else {
                        "malformed".into()
                    },
                    message: format!(
                        "Extension {extension} resolver program is invalid: {message}"
                    ),
                }],
            )
        }
    };
    let secrets = resolver
        .secret_env
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect::<Vec<_>>();
    let request = ExternalCheckDetailRequest {
        schema: EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA.into(),
        provider: provider.into(),
        status: redact(status, &secrets, 512),
        target_url: target_url.map(normalize_target_url),
    };
    match invoke(
        &resolver,
        &extension_path,
        &program,
        &request,
        &secrets,
        deadline,
    ) {
        Ok(response)
            if response.schema == EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA
                && response.provider == provider =>
        {
            (
                vec![HydratedDetail {
                    provider: provider.into(),
                    build_id: response.build_id.map(|value| redact(&value, &secrets, 512)),
                    summary: response
                        .summary
                        .map(|value| redact(&value, &secrets, 2048))
                        .unwrap_or_default(),
                    actions: response
                        .actions
                        .into_iter()
                        .map(|action| redact(&action, &secrets, 512))
                        .collect(),
                    artifact_refs: response
                        .artifact_refs
                        .into_iter()
                        .map(|value| redact(&value, &secrets, 2048))
                        .collect(),
                    log_refs: response
                        .log_refs
                        .into_iter()
                        .map(|value| redact(&value, &secrets, 2048))
                        .collect(),
                }],
                diagnostics,
            )
        }
        Ok(_) => (
            Vec::new(),
            vec![ResolverDiagnostic {
                provider: provider.into(),
                kind: "malformed_identity".into(),
                message: "Resolver response schema or provider did not match its declaration."
                    .into(),
            }],
        ),
        Err(InvocationFailure::MalformedResponse) => (
            Vec::new(),
            vec![ResolverDiagnostic {
                provider: provider.into(),
                kind: "malformed".into(),
                message: "Resolver returned malformed JSON.".into(),
            }],
        ),
        Err(InvocationFailure::Unavailable(message)) => (
            Vec::new(),
            vec![ResolverDiagnostic {
                provider: provider.into(),
                kind: "unavailable".into(),
                message,
            }],
        ),
    }
}

pub(super) fn skipped_for_budget(provider: &str) -> ResolverDiagnostic {
    ResolverDiagnostic {
        provider: provider.into(),
        kind: "budget_exhausted".into(),
        message: "Resolver invocation limit reached; original check evidence was retained.".into(),
    }
}

/// Whether a provider can consume a resolver invocation slot. Invalid resolvers
/// intentionally count: they are an extension-owned failure, unlike unknown or
/// ambiguous providers which cannot invoke anything.
pub(super) fn has_unique_resolver(provider: &str) -> bool {
    load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|manifest| manifest.external_check_detail_resolvers)
        .filter(|resolver| resolver.provider == provider)
        .take(2)
        .count()
        == 1
}

fn invoke(
    resolver: &ExternalCheckDetailResolverConfig,
    extension_path: &Path,
    program: &Path,
    request: &ExternalCheckDetailRequest,
    secrets: &[String],
    deadline: Instant,
) -> Result<ExternalCheckDetailResponse, InvocationFailure> {
    if Instant::now() >= deadline {
        return Err(InvocationFailure::Unavailable(
            "Resolver budget exhausted before spawn.".into(),
        ));
    }
    let mut command = Command::new(program);
    command
        .args(&resolver.command[1..])
        .current_dir(extension_path)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in resolver.public_env.iter().chain(&resolver.secret_env) {
        if let Ok(value) = std::env::var(name) {
            command.env(name, value);
        }
    }
    let mut containment = ProcessContainment::prepare(&mut command).map_err(|error| {
        InvocationFailure::Unavailable(format!("Resolver containment setup failed: {error}"))
    })?;
    let guard = ControllerChildGuard::prepare(&mut command).map_err(|error| {
        InvocationFailure::Unavailable(format!("Resolver containment setup failed: {error}"))
    })?;
    let mut child = command.spawn().map_err(|error| {
        InvocationFailure::Unavailable(format!("Resolver spawn failed: {error}"))
    })?;
    if let Err(error) = containment.attach(&child) {
        cleanup(&containment, &mut child, false);
        return Err(InvocationFailure::Unavailable(format!(
            "Resolver containment attach failed: {error}"
        )));
    }
    if let Err(error) = guard.attach(&child) {
        cleanup(&containment, &mut child, false);
        return Err(InvocationFailure::Unavailable(format!(
            "Resolver containment attach failed: {error}"
        )));
    }
    let mut stdin = child.stdin.take().ok_or_else(|| {
        cleanup(&containment, &mut child, false);
        InvocationFailure::Unavailable("Resolver stdin was unavailable.".into())
    })?;
    let payload = serde_json::to_vec(request).map_err(|error| {
        InvocationFailure::Unavailable(format!("Resolver request serialization failed: {error}"))
    })?;
    if stdin
        .write_all(&payload)
        .and_then(|_| stdin.write_all(b"\n"))
        .is_err()
    {
        cleanup(&containment, &mut child, false);
        return Err(InvocationFailure::Unavailable(
            "Resolver stdin write failed.".into(),
        ));
    }
    drop(stdin);
    let stdout = child.stdout.take().ok_or_else(|| {
        cleanup(&containment, &mut child, false);
        InvocationFailure::Unavailable("Resolver stdout was unavailable.".into())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        cleanup(&containment, &mut child, false);
        InvocationFailure::Unavailable("Resolver stderr was unavailable.".into())
    })?;
    let (tx, rx) = mpsc::channel();
    let stdout_reader = std::thread::spawn(move || {
        let _ = tx.send(read_bounded(stdout));
    });
    let (err_tx, err_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let _ = err_tx.send(read_bounded(stderr));
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A resolver may exit while a descendant still owns a pipe.
                // Its process group is ours, so close that escape before joining
                // the supervised readers.
                cleanup(&containment, &mut child, true);
                if let Err(error) = terminate_remaining_process_group(child.id()) {
                    cleanup(&containment, &mut child, true);
                    join_readers(stdout_reader, stderr_reader);
                    return Err(InvocationFailure::Unavailable(format!(
                        "Resolver descendant cleanup failed: {error}"
                    )));
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                let output = match rx.recv_timeout(remaining) {
                    Ok(output) => output,
                    Err(_) => {
                        // The leader is already reaped. Run full procfs and
                        // group cleanup before joining pipe readers so a
                        // detached session cannot keep this invocation alive.
                        cleanup(&containment, &mut child, true);
                        join_readers(stdout_reader, stderr_reader);
                        return Err(InvocationFailure::Unavailable(
                            "Resolver stdout reader did not finish within the total budget.".into(),
                        ));
                    }
                };
                let stderr = match err_rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(stderr) => stderr,
                    Err(_) => {
                        cleanup(&containment, &mut child, true);
                        join_readers(stdout_reader, stderr_reader);
                        return Err(InvocationFailure::Unavailable(
                            "Resolver stderr reader did not finish within the total budget.".into(),
                        ));
                    }
                };
                join_readers(stdout_reader, stderr_reader);
                if output.len() > MAX_OUTPUT_BYTES || stderr.len() > MAX_OUTPUT_BYTES {
                    return Err(InvocationFailure::Unavailable(
                        "Resolver output exceeded the 64 KiB limit.".into(),
                    ));
                }
                if !status.success() {
                    return Err(InvocationFailure::Unavailable(format!(
                        "Resolver exited unsuccessfully: {}",
                        redact(&String::from_utf8_lossy(&stderr), secrets, 512)
                    )));
                }
                return serde_json::from_slice(&output)
                    .map_err(|_| InvocationFailure::MalformedResponse);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                cleanup(&containment, &mut child, false);
                join_readers(stdout_reader, stderr_reader);
                return Err(InvocationFailure::Unavailable(
                    "Resolver timed out at its deadline; its process tree was terminated. Inspect the extension resolver command and its child-process cleanup."
                        .into(),
                ));
            }
            Err(error) => {
                cleanup(&containment, &mut child, false);
                join_readers(stdout_reader, stderr_reader);
                return Err(InvocationFailure::Unavailable(format!(
                    "Resolver wait failed: {error}"
                )));
            }
        }
    }
}

fn resolve_program(
    extension_path: &Path,
    resolver: &ExternalCheckDetailResolverConfig,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let root = extension_path
        .canonicalize()
        .map_err(|error| format!("extension root cannot be resolved: {error}"))?;
    let program = root
        .join(&resolver.command[0])
        .canonicalize()
        .map_err(|error| format!("program cannot be resolved inside the extension: {error}"))?;
    if !program.starts_with(&root) {
        return Err("program resolves outside the declaring extension".into());
    }
    Ok((root, program))
}

fn read_bounded(mut reader: impl Read) -> Vec<u8> {
    let mut value = Vec::new();
    let _ = reader
        .by_ref()
        .take((MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut value);
    value
}

fn cleanup(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
    leader_has_exited: bool,
) {
    // A resolver can call `setsid` and leave its inherited process group. The
    // current core primitive snapshots Linux procfs descendants before killing
    // the leader, so timeout cleanup still reaches that detached session.
    let _ = containment.terminate_on_failure_bounded(CLEANUP_BUDGET, leader_has_exited);
    let _ = containment.cleanup_after_leader_exit_bounded(CLEANUP_BUDGET);
    let _ = force_terminate_process_tree_bounded(child.id(), CLEANUP_BUDGET);
    let _ = terminate_process_tree_and_reap(child);
}

fn join_readers(stdout: std::thread::JoinHandle<()>, stderr: std::thread::JoinHandle<()>) {
    let _ = stdout.join();
    let _ = stderr.join();
}

pub(super) fn normalize_target_url(value: &str) -> String {
    let without_secret_suffix = value.split(['?', '#']).next().unwrap_or_default();
    let without_credentials = without_secret_suffix
        .split_once("//")
        .map(|(scheme, rest)| format!("{scheme}//{}", rest.rsplit('@').next().unwrap_or(rest)))
        .unwrap_or_else(|| without_secret_suffix.to_string());
    bound(&without_credentials, 2048)
}

fn redact(value: &str, secrets: &[String], limit: usize) -> String {
    let mut redacted = RedactionPolicy::default().redact_embedded_urls(value);
    for secret in secrets {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
    }
    bound(&redacted, limit)
}

fn bound(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
#[test]
fn cross_platform_fixture() {
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
            Some("process tree was terminated"),
        ),
    ] {
        homeboy::core::test_support::with_isolated_home(|_| {
            install_fixture_extension(mode, FIXTURE_MODE_ENV);
            let started = Instant::now();
            let (details, diagnostics) = hydrate(
                "fixture-ci",
                "failure",
                Some("https://example.test/build/42"),
                started + budget,
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{mode}: resolver timeout did not return after process cleanup"
            );
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
                assert!(
                    diagnostics[0].message.contains(expected_message),
                    "{mode}: {diagnostics:?}"
                );
            }
            if mode == "success" {
                assert_eq!(details[0].actions, ["fixture-ci replay 42"]);
                let mut resolver_slots = MAX_RESOLVERS;
                let check = super::failure_log_triage::hydrate_external(
                    "fixture-ci".into(),
                    "failure".into(),
                    Some("legacy failure description".into()),
                    Some("https://example.test/build/42".into()),
                    &mut resolver_slots,
                    Instant::now() + Duration::from_secs(10),
                );
                assert_eq!(check.status, "failure");
                assert_eq!(
                    check.description.as_deref(),
                    Some("legacy failure description")
                );
                let summary = super::failure_log_triage::render_human_summary(
                    "owner/repo",
                    &super::failure_log_triage::GhPullRequest {
                        number: 42,
                        title: "Fixture".into(),
                        url: "https://example.test/pull/42".into(),
                        head_sha: "abc".into(),
                    },
                    0,
                    &[],
                    &[check],
                    &[],
                    &[],
                );
                assert!(summary.contains("fixture-ci replay 42"));
            }
        });
    }
    homeboy::core::test_support::with_isolated_home(|_| {
        let (details, diagnostics) = hydrate(
            "no-installed-provider",
            "error",
            None,
            Instant::now() + Duration::from_secs(10),
        );
        assert!(details.is_empty());
        assert_eq!(diagnostics[0].kind, "unknown");
    });
}

#[cfg(test)]
fn install_fixture_extension(mode: &str, fixture_mode_env: &str) {
    let root = homeboy::core::paths::homeboy().unwrap();
    let extension = root.join("extensions").join("fixture-external-check");
    std::fs::create_dir_all(&extension).unwrap();
    let executable = fixture_program(&extension, fixture_mode_env);
    let command = if mode == "missing-executable" {
        "not-installed-resolver".to_string()
    } else {
        executable
    };
    std::fs::write(
        extension.join("fixture-external-check.json"),
        serde_json::json!({
            "name": "Fixture external check",
            "version": "1.0.0",
            "external_check_detail_resolvers": [{
                "schema": "homeboy/external-check-detail-resolver/v1",
                "provider": "fixture-ci",
                "command": [command],
                "public_env": [fixture_mode_env]
            }]
        })
        .to_string(),
    )
    .unwrap();
    std::env::set_var(fixture_mode_env, mode);
}

#[cfg(all(test, unix))]
fn fixture_program(extension: &Path, fixture_mode_env: &str) -> String {
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
    name.into()
}

#[cfg(all(test, windows))]
fn fixture_program(extension: &Path, fixture_mode_env: &str) -> String {
    let name = "fixture-resolver.cmd";
    let path = extension.join(name);
    std::fs::write(
        &path,
        format!(
            "@echo off\r\nif \"%{fixture_mode_env}%\"==\"success\" (echo {{\"schema\":\"homeboy/external-check-detail-response/v1\",\"provider\":\"fixture-ci\",\"summary\":\"fixture hydrated failure\",\"actions\":[\"fixture-ci replay 42\"]}}& exit /b 0)\r\nif \"%{fixture_mode_env}%\"==\"malformed\" (set /p =not json<nul& exit /b 0)\r\nif \"%{fixture_mode_env}%\"==\"unavailable\" exit /b 23\r\nif \"%{fixture_mode_env}%\"==\"timeout\" timeout /t 30 /nobreak >nul\r\nexit /b 24\r\n"
        ),
    )
    .unwrap();
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_url_strips_fragments_and_credentials() {
        assert_eq!(
            normalize_target_url("https://user:token@example.test/job/1?token=secret#secret"),
            "https://example.test/job/1"
        );
    }

    #[test]
    fn response_identity_is_a_separate_contract() {
        let response: ExternalCheckDetailResponse = serde_json::from_str(r#"{"schema":"homeboy/external-check-detail-response/v1","provider":"example","summary":"failed"}"#).unwrap();
        assert_eq!(response.provider, "example");
        assert!(response.actions.is_empty());
    }

    #[test]
    fn redaction_removes_projected_secrets_and_url_suffixes() {
        assert_eq!(
            redact(
                "token=secret https://user:pass@example.test/log?token=secret#fragment",
                &["secret".into()],
                512,
            ),
            "token=[REDACTED] https://[REDACTED]@example.test/log?token=[REDACTED]"
        );
    }

    #[test]
    fn resolver_program_is_canonicalized_inside_its_extension() {
        let extension = tempfile::tempdir().unwrap();
        let script = std::env::current_exe().unwrap();
        std::fs::copy(&script, extension.path().join("resolve")).unwrap();
        let config = ExternalCheckDetailResolverConfig {
            schema: "homeboy/external-check-detail-resolver/v1".into(),
            provider: "fixture".into(),
            command: vec!["resolve".into()],
            public_env: Vec::new(),
            secret_env: Vec::new(),
        };
        let (root, program) = resolve_program(extension.path(), &config).unwrap();
        assert_eq!(root, extension.path().canonicalize().unwrap());
        assert!(program.starts_with(root));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_reaps_a_detached_session_descendant() {
        use std::os::unix::fs::PermissionsExt;

        let extension = tempfile::tempdir().unwrap();
        let pid_file = extension.path().join("descendant.pid");
        let script = extension.path().join("resolve");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nperl -MPOSIX -e 'my $child = fork; die \"fork: $!\\n\" unless defined $child; if ($child) {{ wait; exit }} POSIX::setsid() or die \"setsid: $!\\n\"; open my $pid, \">\", $ARGV[0] or die $!; print $pid $$; close $pid; sleep 30' {} &\nwhile [ ! -s {} ]; do :; done\nsleep 30\n",
                homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
                pid_file.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolver = ExternalCheckDetailResolverConfig {
            schema: EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA.into(),
            provider: "fixture".into(),
            command: vec!["resolve".into()],
            public_env: Vec::new(),
            secret_env: Vec::new(),
        };
        let request = ExternalCheckDetailRequest {
            schema: EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA.into(),
            provider: "fixture".into(),
            status: "failed".into(),
            target_url: None,
        };

        let error = invoke(
            &resolver,
            extension.path(),
            &script,
            &request,
            &[],
            Instant::now() + Duration::from_secs(1),
        )
        .expect_err("resolver must time out");
        assert!(matches!(
            error,
            InvocationFailure::Unavailable(ref message) if message.contains("process tree was terminated")
        ));
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            !homeboy::core::process::pid_is_running(descendant_pid),
            "detached resolver descendant {descendant_pid} survived timeout cleanup"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn leader_exit_reaps_a_detached_session_pipe_holder() {
        use std::os::unix::fs::PermissionsExt;

        let extension = tempfile::tempdir().unwrap();
        let pid_file = extension.path().join("descendant.pid");
        let script = extension.path().join("resolve");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsetsid sh -c 'echo $$ > \"$1\"; sleep 30' sh {} &\nwhile [ ! -s {} ]; do :; done\nexit 0\n",
                homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
                homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolver = ExternalCheckDetailResolverConfig {
            schema: EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA.into(),
            provider: "fixture".into(),
            command: vec!["resolve".into()],
            public_env: Vec::new(),
            secret_env: Vec::new(),
        };
        let request = ExternalCheckDetailRequest {
            schema: EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA.into(),
            provider: "fixture".into(),
            status: "failed".into(),
            target_url: None,
        };

        let error = invoke(
            &resolver,
            extension.path(),
            &script,
            &request,
            &[],
            Instant::now() + Duration::from_millis(100),
        )
        .expect_err("inherited pipes must not outlive the resolver deadline");
        assert!(matches!(
            error,
            InvocationFailure::Unavailable(ref message) if message.contains("reader did not finish")
        ));
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            !homeboy::core::process::pid_is_running(descendant_pid),
            "detached resolver descendant {descendant_pid} survived leader-exit cleanup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_resolver_outside_extension_is_rejected_before_projection() {
        use std::os::unix::fs::symlink;
        let extension = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), extension.path().join("resolve")).unwrap();
        let config = ExternalCheckDetailResolverConfig {
            schema: homeboy::extension::EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA.into(),
            provider: "fixture".into(),
            command: vec!["resolve".into()],
            public_env: vec![],
            secret_env: vec!["TOKEN".into()],
        };
        assert!(resolve_program(extension.path(), &config).is_err());
    }
}
