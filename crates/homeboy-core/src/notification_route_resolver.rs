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

use crate::{extension_store::load_all_extensions, notification_route::NotificationRoute};

const AMBIENT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const RESOLVER_OUTPUT_LIMIT: usize = 16 * 1024;

/// Ask installed transport resolvers for a route. One match is selected; zero
/// matches retains route-less behavior. Ambient discovery has one aggregate
/// deadline, so optional resolvers cannot delay durable submission per install.
/// A resolver that cannot start or exceeds that deadline is skipped with a
/// bounded diagnostic; malformed, invalid, and ambiguous results fail closed.
pub fn resolve_installed() -> Result<Option<NotificationRoute>> {
    resolve_installed_with_timeout(AMBIENT_DISCOVERY_TIMEOUT)
}

fn resolve_installed_with_timeout(timeout: Duration) -> Result<Option<NotificationRoute>> {
    let extensions = load_all_extensions()?;
    let mut matches = Vec::new();
    let started = Instant::now();
    for extension in extensions {
        for transport in &extension.notification_transports {
            let Some(resolver) = &transport.route_resolver else {
                continue;
            };
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                ambient_discovery_warning("notification route resolver discovery timed out");
                return select_match(matches);
            };
            match invoke(
                resolver,
                &transport.id,
                extension.extension_path.as_deref().unwrap_or_default(),
                remaining,
            ) {
                Ok(Some(route)) => matches.push(route),
                Ok(None) => {}
                Err(InvokeError::Optional(message)) => {
                    ambient_discovery_warning(message);
                    if started.elapsed() >= timeout {
                        return select_match(matches);
                    }
                }
                Err(InvokeError::Fatal(error)) => return Err(error),
            }
        }
    }
    select_match(matches)
}

fn select_match(mut matches: Vec<NotificationRoute>) -> Result<Option<NotificationRoute>> {
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
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

enum InvokeError {
    Optional(&'static str),
    Fatal(Error),
}

fn invoke(
    resolver: &NotificationRouteResolverConfig,
    transport: &str,
    extension_path: &str,
    timeout: Duration,
) -> std::result::Result<Option<NotificationRoute>, InvokeError> {
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
    match (response.status, response.route) {
        (NotificationRouteResolverStatus::Unmatched, None) => Ok(None),
        (NotificationRouteResolverStatus::Matched, Some(route)) => {
            NotificationRoute::new(transport, route)
                .map(Some)
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
    Error::validation_invalid_argument("notification_route_resolver", message, None, None)
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
        crate::extension_store::save_manifest(&manifest).unwrap();
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
            install_resolver("resolver-test", "printf not-json");
            assert!(resolve_installed()
                .unwrap_err()
                .to_string()
                .contains("malformed"));
        });
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "yes x | head -c 20000");
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
            let matched = "printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"matched\",\"route\":\"route\"}'";
            install_resolver("resolver-one", matched);
            install_resolver("resolver-two", matched);
            assert!(resolve_installed()
                .unwrap_err()
                .to_string()
                .contains("more than one"));
        });
        crate::test_support::with_isolated_home(|_| {
            install_resolver("resolver-test", "printf '%s' '{\"schema\":\"homeboy/notification-route-resolver/v1\",\"status\":\"matched\",\"route\":\"token=secret\"}'");
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
                resolve_installed_with_timeout(Duration::from_millis(100)).unwrap(),
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
}
