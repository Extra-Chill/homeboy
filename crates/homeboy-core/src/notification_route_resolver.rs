//! Extension-owned notification route resolution.
//!
//! Core supplies only the transport identity. Resolvers own all interpretation
//! of their inherited caller context and return a small, versioned JSON result.

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use homeboy_error::{Error, Result};
use homeboy_extension_contract::{
    NotificationRouteResolverConfig, NotificationRouteResolverRequest,
    NotificationRouteResolverResponse, NotificationRouteResolverStatus,
    NOTIFICATION_ROUTE_RESOLVER_REQUEST_SCHEMA, NOTIFICATION_ROUTE_RESOLVER_SCHEMA,
};

use crate::{
    extension::catalog::load_all_extensions,
    notification_route::{NotificationRoute, NotificationRouteResolution},
};

const AMBIENT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const RESOLVER_OUTPUT_LIMIT: usize = 16 * 1024;
const MAX_MISSING_CONTEXT_FIELDS: usize = 32;

/// Ask installed transport resolvers for a route. One match is selected; zero
/// matches retains route-less behavior. Ambient discovery has one aggregate
/// deadline, so optional resolvers cannot delay durable submission per install.
/// A resolver that cannot start or exceeds that deadline is skipped with a
/// bounded diagnostic; malformed, invalid, and ambiguous results fail closed.
pub fn resolve_installed() -> Result<Option<NotificationRoute>> {
    Ok(resolve_installed_with_evidence()?.route)
}

/// Resolve installed extension routes while retaining safe admission evidence.
pub fn resolve_installed_with_evidence() -> Result<ResolvedNotificationRoute> {
    resolve_installed_with_timeout(AMBIENT_DISCOVERY_TIMEOUT)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNotificationRoute {
    pub route: Option<NotificationRoute>,
    pub evidence: NotificationRouteResolution,
}

fn resolve_installed_with_timeout(timeout: Duration) -> Result<ResolvedNotificationRoute> {
    let extensions = load_all_extensions()?;
    let mut matches = Vec::new();
    let mut missing_context = Vec::new();
    let mut resolver_transports = Vec::new();
    let started = Instant::now();
    for extension in extensions {
        for transport in &extension.notification_transports {
            let Some(resolver) = &transport.route_resolver else {
                continue;
            };
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                ambient_discovery_warning("notification route resolver discovery timed out");
                return select_match(matches, resolver_transports, missing_context);
            };
            match invoke(
                resolver,
                &transport.id,
                extension.extension_path.as_deref().unwrap_or_default(),
                remaining,
            ) {
                Ok(ResolverResult::Matched(route)) => matches.push(route),
                Ok(ResolverResult::Unmatched {
                    missing_context: missing,
                }) => {
                    resolver_transports.push(transport.id.clone());
                    missing_context.extend(missing);
                }
                Err(InvokeError::Optional(message)) => {
                    ambient_discovery_warning(message);
                    if started.elapsed() >= timeout {
                        return select_match(matches, resolver_transports, missing_context);
                    }
                }
                Err(InvokeError::Fatal(error)) => return Err(error),
            }
        }
    }
    select_match(matches, resolver_transports, missing_context)
}

fn select_match(
    mut matches: Vec<NotificationRoute>,
    resolver_transports: Vec<String>,
    mut missing_context: Vec<String>,
) -> Result<ResolvedNotificationRoute> {
    match matches.len() {
        0 => {
            missing_context.sort();
            missing_context.dedup();
            let mut evidence = NotificationRouteResolution::new("route_less");
            evidence.resolver_transport =
                (resolver_transports.len() == 1).then(|| resolver_transports[0].clone());
            evidence.missing_context = missing_context;
            Ok(ResolvedNotificationRoute {
                route: None,
                evidence,
            })
        }
        1 => {
            let route = matches.pop();
            let mut evidence = NotificationRouteResolution::new("resolver");
            evidence.transport = route.as_ref().map(|route| route.transport.clone());
            evidence.resolver_transport = evidence.transport.clone();
            Ok(ResolvedNotificationRoute { route, evidence })
        }
        _ => Err(resolver_error(
            "more than one installed notification route resolver matched",
        )),
    }
}

/// Apply the explicit route precedence contract. Ambient resolver execution is
/// intentionally owned by the Cook submission path, after request validation.
pub fn resolve_from_cli_or_env(
    cli_transport: Option<&str>,
    cli_route: Option<&str>,
) -> Result<Option<NotificationRoute>> {
    match crate::notification_route::from_cli_or_env(cli_transport, cli_route)? {
        Some(route) => Ok(Some(route)),
        None => Ok(None),
    }
}

/// Resolve explicit argv or propagated environment context with its source.
pub fn resolve_from_cli_or_env_with_evidence(
    cli_transport: Option<&str>,
    cli_route: Option<&str>,
) -> Result<ResolvedNotificationRoute> {
    let route = resolve_from_cli_or_env(cli_transport, cli_route)
        .map_err(with_invalid_resolution_evidence)?;
    let mut evidence = NotificationRouteResolution::new(if cli_transport.is_some() {
        "explicit"
    } else if route.is_some() {
        "environment"
    } else {
        "route_less"
    });
    evidence.transport = route.as_ref().map(|route| route.transport.clone());
    if cli_transport.is_none() {
        if let Some(propagated) =
            crate::notification_route::propagated_resolution_from_env(route.as_ref())
        {
            evidence = propagated;
        }
    }
    Ok(ResolvedNotificationRoute { route, evidence })
}

enum InvokeError {
    Optional(&'static str),
    Fatal(Error),
}

enum ResolverResult {
    Matched(NotificationRoute),
    Unmatched { missing_context: Vec<String> },
}

fn invoke(
    resolver: &NotificationRouteResolverConfig,
    transport: &str,
    extension_path: &str,
    timeout: Duration,
) -> std::result::Result<ResolverResult, InvokeError> {
    let argv: Vec<String> = resolver
        .command
        .iter()
        .map(|arg| arg.replace("{extension_path}", extension_path))
        .collect();
    let program = argv.first().expect("validated resolver command");
    let request = serde_json::to_vec(&NotificationRouteResolverRequest {
        schema: NOTIFICATION_ROUTE_RESOLVER_REQUEST_SCHEMA.to_string(),
        transport: transport.to_string(),
    })
    .expect("resolver request is serializable");
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| {
        InvokeError::Optional("could not start an installed notification route resolver")
    })?;
    if child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(&request)
        .is_err()
    {
        terminate(&mut child, None);
        return Err(InvokeError::Fatal(resolver_error(
            "could not send the resolver request",
        )));
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || read_bounded(stdout));
    let started = Instant::now();
    let status = loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(_) => {
                terminate(&mut child, Some(reader));
                return Err(InvokeError::Fatal(resolver_error(
                    "could not wait for a notification route resolver",
                )));
            }
        };
        if let Some(status) = status {
            // A resolver parent can exit while a descendant retains stdout.
            // Reap its group before joining the reader so that cannot bypass
            // the aggregate deadline by withholding EOF.
            kill_process_group(&mut child);
            break status;
        }
        if started.elapsed() >= timeout {
            terminate(&mut child, Some(reader));
            return Err(InvokeError::Optional(
                "notification route resolver discovery timed out",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (stdout, oversized) = reader.join().map_err(|_| {
        InvokeError::Fatal(resolver_error(
            "notification route resolver output reader failed",
        ))
    })?;
    if oversized {
        return Err(InvokeError::Fatal(resolver_error(
            "notification route resolver output exceeded its limit",
        )));
    }
    if !status.success() {
        return Err(InvokeError::Fatal(resolver_error(
            "notification route resolver exited unsuccessfully",
        )));
    }
    let response: NotificationRouteResolverResponse =
        serde_json::from_slice(&stdout).map_err(|_| {
            InvokeError::Fatal(resolver_error(
                "notification route resolver returned malformed output",
            ))
        })?;
    if response.schema != NOTIFICATION_ROUTE_RESOLVER_SCHEMA {
        return Err(InvokeError::Fatal(resolver_error(
            "notification route resolver returned an unsupported schema",
        )));
    }
    if response.missing_context.len() > MAX_MISSING_CONTEXT_FIELDS
        || response
            .missing_context
            .iter()
            .any(|field| !valid_context_field_name(field))
    {
        return Err(InvokeError::Fatal(resolver_error(
            "notification route resolver returned invalid missing context field names",
        )));
    }
    match (response.status, response.route) {
        (NotificationRouteResolverStatus::Unmatched, None) => Ok(ResolverResult::Unmatched {
            missing_context: response.missing_context,
        }),
        (NotificationRouteResolverStatus::Matched, Some(route))
            if response.missing_context.is_empty() =>
        {
            NotificationRoute::new(transport, route)
                .map(ResolverResult::Matched)
                .map_err(|_| {
                    InvokeError::Fatal(resolver_error(
                        "notification route resolver returned an invalid route",
                    ))
                })
        }
        _ => Err(InvokeError::Fatal(resolver_error(
            "notification route resolver returned an invalid result shape",
        ))),
    }
}

fn valid_context_field_name(field: &str) -> bool {
    !field.is_empty()
        && field.len() <= 128
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn terminate(child: &mut std::process::Child, reader: Option<thread::JoinHandle<(Vec<u8>, bool)>>) {
    kill_process_group(child);
    let _ = child.wait();
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

/// Kill the resolver's whole process group so grandchildren cannot retain its
/// stdout pipe after the aggregate deadline has expired.
#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) {
    if child.id() <= i32::MAX as u32 {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    } else {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn ambient_discovery_warning(message: &str) {
    eprintln!("Warning: {message}; continuing without an ambient notification route.");
}

fn read_bounded(mut reader: impl Read) -> (Vec<u8>, bool) {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut oversized = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let remaining = RESOLVER_OUTPUT_LIMIT
                    .saturating_add(1)
                    .saturating_sub(output.len());
                output.extend_from_slice(&buffer[..count.min(remaining)]);
                oversized |= output.len() > RESOLVER_OUTPUT_LIMIT || count > remaining;
            }
        }
    }
    (output, oversized)
}

fn resolver_error(message: &str) -> Error {
    with_invalid_resolution_evidence(Error::validation_invalid_argument(
        "notification_route_resolver",
        message,
        None,
        None,
    ))
}

fn with_invalid_resolution_evidence(mut error: Error) -> Error {
    if let Some(details) = error.details.as_object_mut() {
        details.insert(
            "notification_resolution".to_string(),
            serde_json::to_value(NotificationRouteResolution::new("invalid"))
                .expect("notification resolution is serializable"),
        );
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn install_resolver(id: &str, command: &str) {
        install_resolver_command(id, vec!["sh", "-c", command]);
    }

    #[cfg(unix)]
    fn install_resolver_command(id: &str, command: Vec<&str>) {
        let mut manifest: homeboy_extension_contract::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Resolver test",
                "version": "1.0.0",
                "notification_transports": [{
                    "id": "synthetic.completed",
                    "command": ["true"],
                    "route_resolver": { "command": command }
                }]
            }))
            .unwrap();
        manifest.id = id.to_string();
        crate::extension::catalog::save_manifest(&manifest).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn selects_one_transport_neutral_resolver_match() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "request=$(cat); [ \"$request\" = '{\"schema\":\"homeboy/notification-route-resolver-request/v1\",\"transport\":\"synthetic.completed\"}' ] || exit 9; printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"matched\",\"route\":\"opaque-thread\"}'");
            assert_eq!(
                resolve_installed().unwrap(),
                Some(NotificationRoute::new("synthetic.completed", "opaque-thread").unwrap())
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn unmatched_resolver_preserves_route_less_behavior() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"unmatched\"}'");
            assert_eq!(resolve_installed().unwrap(), None);
        });
    }

    #[cfg(unix)]
    #[test]
    fn ambient_start_failures_preserve_route_less_behavior() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver_command("resolver-test", vec!["/definitely/not/a/resolver"]);
            assert_eq!(resolve_installed().unwrap(), None);
        });
    }

    #[cfg(unix)]
    #[test]
    fn malformed_and_oversized_results_fail_closed() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "cat >/dev/null; printf not-json");
            assert!(resolve_installed()
                .unwrap_err()
                .to_string()
                .contains("malformed"));
        });
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "cat >/dev/null; yes x | head -c 20000");
            assert!(resolve_installed()
                .unwrap_err()
                .to_string()
                .contains("exceeded"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn ambiguity_and_invalid_routes_fail_closed() {
        crate::test_support::with_isolated_home(|_| {
            let matched = "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"matched\",\"route\":\"route\"}'";
            install_resolver("resolver-one", matched);
            install_resolver("resolver-two", matched);
            assert!(resolve_installed()
                .unwrap_err()
                .to_string()
                .contains("more than one"));
        });
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"matched\",\"route\":\"token=secret\"}'");
            assert!(resolve_installed()
                .unwrap_err()
                .to_string()
                .contains("invalid route"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn ambient_discovery_uses_one_deadline_and_reaps_process_groups() {
        crate::test_support::with_isolated_home(|home| {
            let marker = home.path().join("resolver-descendant-ran");
            let command = format!("(sleep 1; touch {}) & wait", marker.display());
            install_resolver("resolver-one", &command);
            install_resolver("resolver-two", "sleep 1");
            let started = Instant::now();
            assert_eq!(
                resolve_installed_with_timeout(Duration::from_millis(100))
                    .unwrap()
                    .route,
                None
            );
            assert!(started.elapsed() < Duration::from_millis(500));
            thread::sleep(Duration::from_secs(1));
            assert!(!marker.exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn explicit_route_wins_without_invoking_a_resolver() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "sleep 3");
            let started = Instant::now();
            let route = resolve_from_cli_or_env(Some("chosen.transport"), Some("chosen-route"))
                .expect("explicit route is valid");
            assert!(started.elapsed() < Duration::from_secs(1));
            assert_eq!(
                route,
                Some(NotificationRoute::new("chosen.transport", "chosen-route").unwrap())
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn unmatched_resolver_reports_safe_missing_caller_context() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver(
                "resolver-test",
                "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"unmatched\",\"missing_context\":[\"CALLER_THREAD_ID\"]}'",
            );
            let resolution = resolve_installed_with_evidence().unwrap();
            assert!(resolution.route.is_none());
            assert_eq!(resolution.evidence.classification, "route_less");
            assert_eq!(
                resolution.evidence.resolver_transport.as_deref(),
                Some("synthetic.completed")
            );
            assert_eq!(resolution.evidence.missing_context, ["CALLER_THREAD_ID"]);
            assert!(serde_json::to_string(&resolution.evidence)
                .unwrap()
                .contains("CALLER_THREAD_ID"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_missing_context_diagnostics_fail_closed() {
        crate::test_support::with_isolated_home(|_| {
            install_resolver(
                "resolver-test",
                "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"unmatched\",\"missing_context\":[\"token=opaque-destination\"]}'",
            );
            let error = resolve_installed_with_evidence().unwrap_err();
            assert!(error.to_string().contains("invalid missing context"));
            assert!(!error.to_string().contains("opaque-destination"));
        });
    }

    #[test]
    fn explicit_and_environment_context_have_distinct_evidence() {
        let explicit = resolve_from_cli_or_env_with_evidence(Some("chosen"), Some("route"))
            .expect("explicit route");
        assert_eq!(explicit.evidence.classification, "explicit");
        assert_eq!(explicit.evidence.transport.as_deref(), Some("chosen"));

        let invalid = resolve_from_cli_or_env_with_evidence(Some("chosen"), None).unwrap_err();
        assert_eq!(
            invalid.details["notification_resolution"]["classification"],
            "invalid"
        );
    }
}
