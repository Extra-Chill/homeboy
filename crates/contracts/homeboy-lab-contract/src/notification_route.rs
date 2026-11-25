//! Transport-neutral, run-scoped notification routing.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};

use homeboy_error::{Error, Result};

pub const NOTIFICATION_ROUTE_METADATA_KEY: &str = "notification_route";
pub const NOTIFICATION_RESOLUTION_METADATA_KEY: &str = "notification_resolution";
pub const NOTIFICATION_TRANSPORT_ENV: &str = "HOMEBOY_NOTIFICATION_TRANSPORT";
pub const NOTIFICATION_ROUTE_ENV: &str = "HOMEBOY_NOTIFICATION_ROUTE";
pub const NOTIFICATION_RESOLUTION_ENV: &str = "HOMEBOY_NOTIFICATION_RESOLUTION";

thread_local! {
    static CURRENT_NOTIFICATION_ROUTE: RefCell<Option<NotificationRoute>> = const { RefCell::new(None) };
    static CURRENT_NOTIFICATION_RESOLUTION: RefCell<Option<NotificationRouteResolution>> = const { RefCell::new(None) };
}

/// Safe evidence of how a notification route was selected. This intentionally
/// never includes the opaque route value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationRouteResolution {
    pub schema: String,
    pub classification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver_transport: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_context: Vec<String>,
}

impl NotificationRouteResolution {
    pub fn new(classification: impl Into<String>) -> Self {
        Self {
            schema: "homeboy/notification-route-resolution/v1".to_string(),
            classification: classification.into(),
            transport: None,
            resolver_transport: None,
            missing_context: Vec::new(),
        }
    }

    pub fn insert_into_metadata(&self, metadata: &mut serde_json::Value) {
        if !metadata.is_object() {
            *metadata = serde_json::json!({});
        }
        metadata[NOTIFICATION_RESOLUTION_METADATA_KEY] =
            serde_json::to_value(self).expect("notification resolution is serializable");
    }

    pub fn validate(&self) -> bool {
        self.schema == "homeboy/notification-route-resolution/v1"
            && matches!(
                self.classification.as_str(),
                "explicit" | "environment" | "resolver" | "route_less" | "invalid"
            )
            && self.transport.as_deref().is_none_or(valid_transport_id)
            && self
                .resolver_transport
                .as_deref()
                .is_none_or(valid_transport_id)
            && self.missing_context.len() <= 32
            && self
                .missing_context
                .iter()
                .all(|field| valid_safe_identifier(field, 128))
    }
}

/// An opaque, non-secret destination owned by an installed notification transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRoute {
    pub transport: String,
    pub route: String,
}

impl NotificationRoute {
    pub fn new(transport: impl Into<String>, route: impl Into<String>) -> Result<Self> {
        let route = Self {
            transport: transport.into(),
            route: route.into(),
        };
        route.validate()?;
        Ok(route)
    }

    pub fn validate(&self) -> Result<()> {
        let valid_transport = valid_transport_id(&self.transport);
        if !valid_transport {
            return Err(Error::validation_invalid_argument(
                "notification_transport",
                "must contain 1-64 ASCII letters, digits, '.', '_' or '-'",
                Some(self.transport.clone()),
                None,
            ));
        }
        if self.route.is_empty()
            || self.route.len() > 4096
            || self.route.chars().any(char::is_control)
            || contains_credential_syntax(&self.route)
        {
            return Err(Error::validation_invalid_argument(
                "notification_route",
                "must be a non-empty, at most 4096-character opaque non-secret value without control characters or credential syntax",
                None,
                None,
            ));
        }
        Ok(())
    }

    pub fn from_metadata(metadata: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(metadata.get(NOTIFICATION_ROUTE_METADATA_KEY)?.clone())
            .ok()
            .filter(|route: &Self| route.validate().is_ok())
    }

    pub fn insert_into_metadata(&self, metadata: &mut serde_json::Value) {
        if !metadata.is_object() {
            *metadata = serde_json::json!({});
        }
        metadata[NOTIFICATION_ROUTE_METADATA_KEY] =
            serde_json::to_value(self).expect("notification route is serializable");
    }
}

/// Caller-supplied notification context, before route-resolver consultation.
///
/// A transport is enough context to act on by itself: an installed extension
/// may still declare a route resolver that derives a route for it, and a
/// transport with no resolver (or an unmatched one) simply delivers to its
/// own default destination. A route is never meaningful without the transport
/// that owns it, so a route can only appear paired with a transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationRouteContext {
    /// Neither a transport nor a route was supplied.
    None,
    /// A transport was supplied without a route.
    TransportOnly(String),
    /// A transport and a route were supplied together and validated.
    Route(NotificationRoute),
}

impl NotificationRouteContext {
    /// The validated route, when both transport and route were supplied.
    pub fn into_route(self) -> Option<NotificationRoute> {
        match self {
            Self::Route(route) => Some(route),
            Self::None | Self::TransportOnly(_) => None,
        }
    }

    /// The transport identity, if any context was supplied at all.
    pub fn transport(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::TransportOnly(transport) => Some(transport),
            Self::Route(route) => Some(&route.transport),
        }
    }
}

/// Resolve generic caller context once for a process. Explicit CLI values win;
/// environment context is considered only when neither CLI value was supplied.
/// A transport alone is valid context; a route without a transport is not.
pub fn from_cli_or_env(
    cli_transport: Option<&str>,
    cli_route: Option<&str>,
) -> Result<NotificationRouteContext> {
    if let Some(route) = cli_route {
        return match cli_transport {
            Some(transport) => {
                NotificationRoute::new(transport, route).map(NotificationRouteContext::Route)
            }
            None => Err(Error::validation_invalid_argument(
                "notification_route",
                "--notification-route requires --notification-transport",
                None,
                None,
            )),
        };
    }
    if let Some(transport) = cli_transport {
        return validated_transport_only(transport);
    }
    match (
        std::env::var(NOTIFICATION_TRANSPORT_ENV).ok(),
        std::env::var(NOTIFICATION_ROUTE_ENV).ok(),
    ) {
        (None, None) => Ok(NotificationRouteContext::None),
        (Some(transport), Some(route)) => {
            NotificationRoute::new(transport, route).map(NotificationRouteContext::Route)
        }
        (Some(transport), None) => validated_transport_only(&transport),
        (None, Some(_)) => Err(Error::validation_invalid_argument(
            "notification_route",
            format!("{NOTIFICATION_ROUTE_ENV} requires {NOTIFICATION_TRANSPORT_ENV}"),
            None,
            None,
        )),
    }
}

fn validated_transport_only(transport: &str) -> Result<NotificationRouteContext> {
    if !valid_transport_id(transport) {
        return Err(Error::validation_invalid_argument(
            "notification_transport",
            "must contain 1-64 ASCII letters, digits, '.', '_' or '-'",
            Some(transport.to_string()),
            None,
        ));
    }
    Ok(NotificationRouteContext::TransportOnly(
        transport.to_string(),
    ))
}

/// The environment pairs that carry a route across a process boundary.
///
/// This is the producer half of [`from_cli_or_env`]. Both variables were read
/// from the start but nothing ever wrote them, so a route reached a child only
/// when the child's argv happened to preserve it — which is why cook needed a
/// durable-record fallback (#11115). That fallback stays; this is the direct
/// path for the offloaded case it was invented for.
///
/// Emitted together or not at all. [`from_cli_or_env`] never parses a route
/// without a transport, so a producer that set only the route variable would
/// make every child fail to start; a transport-only producer intentionally
/// carries nothing here, since [`NotificationRouteContext::TransportOnly`]
/// still lacks a route to propagate.
///
/// # Credential safety
///
/// The propagated value is the transport-owned destination, documented as an
/// opaque non-secret value and rejected by [`NotificationRoute::validate`] if it
/// carries credential syntax. It is the same string `notify::dispatch` already
/// passes to a transport as a `--route` argv element, and on Linux argv is the
/// *more* exposed channel of the two (`/proc/<pid>/cmdline` is world-readable;
/// `/proc/<pid>/environ` is readable only by the process owner). Propagating it
/// through the environment therefore adds no exposure the delivery path did not
/// already have.
pub fn child_env(route: Option<&NotificationRoute>) -> Vec<(&'static str, String)> {
    let mut environment = route
        .map(|route| {
            vec![
                (NOTIFICATION_TRANSPORT_ENV, route.transport.clone()),
                (NOTIFICATION_ROUTE_ENV, route.route.clone()),
            ]
        })
        .unwrap_or_default();
    if let Some(resolution) = current_resolution().filter(NotificationRouteResolution::validate) {
        environment.push((
            NOTIFICATION_RESOLUTION_ENV,
            serde_json::to_string(&resolution).expect("notification resolution is serializable"),
        ));
    }
    environment
}

pub fn propagated_resolution_from_env(
    route: Option<&NotificationRoute>,
) -> Option<NotificationRouteResolution> {
    let resolution = serde_json::from_str::<NotificationRouteResolution>(
        &std::env::var(NOTIFICATION_RESOLUTION_ENV).ok()?,
    )
    .ok()?;
    let matching_transport = match (resolution.transport.as_deref(), route) {
        (Some(transport), Some(route)) => transport == route.transport,
        (None, None) => true,
        _ => false,
    };
    (resolution.validate() && matching_transport).then_some(resolution)
}

fn valid_transport_id(transport: &str) -> bool {
    valid_safe_identifier(transport, 64)
}

fn valid_safe_identifier(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn contains_credential_syntax(route: &str) -> bool {
    let lowercase = route.to_ascii_lowercase();
    lowercase.contains("authorization=")
        || lowercase.contains("password=")
        || lowercase.contains("secret=")
        || lowercase.contains("token=")
        || lowercase
            .split_once("://")
            .is_some_and(|(_, remainder)| remainder.contains('@'))
}

/// Run work with a route bound only to the current execution thread.
pub fn with_current<T>(route: Option<NotificationRoute>, operation: impl FnOnce() -> T) -> T {
    CURRENT_NOTIFICATION_ROUTE.with(|current| {
        let previous = current.replace(route);
        let result = operation();
        current.replace(previous);
        result
    })
}

pub fn current() -> Option<NotificationRoute> {
    CURRENT_NOTIFICATION_ROUTE.with(|current| current.borrow().clone())
}

/// Bind route-resolution evidence for the command lifetime.
pub fn with_current_resolution<T>(
    resolution: Option<NotificationRouteResolution>,
    operation: impl FnOnce() -> T,
) -> T {
    CURRENT_NOTIFICATION_RESOLUTION.with(|current| {
        let previous = current.replace(resolution);
        let result = operation();
        current.replace(previous);
        result
    })
}

pub fn current_resolution() -> Option<NotificationRouteResolution> {
    CURRENT_NOTIFICATION_RESOLUTION.with(|current| current.borrow().clone())
}

/// A route captured from one thread so it can be re-bound on another.
///
/// The current route is thread-local, so work moved onto a worker thread
/// (`thread::spawn`, `thread::scope`) starts with no route and silently stops
/// attributing its runs to the caller's destination. Capture on the parent
/// thread and re-bind inside the child body.
#[derive(Debug, Clone, Default)]
pub struct PropagatedNotificationRoute(Option<NotificationRoute>);

impl PropagatedNotificationRoute {
    /// Re-bind the captured route for the duration of `operation`.
    pub fn bind<T>(&self, operation: impl FnOnce() -> T) -> T {
        with_current(self.0.clone(), operation)
    }

    pub fn route(&self) -> Option<&NotificationRoute> {
        self.0.as_ref()
    }
}

/// Capture the calling thread's route for propagation onto worker threads.
pub fn capture() -> PropagatedNotificationRoute {
    PropagatedNotificationRoute(current())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Run `operation` with the route environment pair set exactly as given,
    /// restoring the previous values afterwards.
    fn with_route_env<T>(
        transport: Option<&str>,
        route: Option<&str>,
        operation: impl FnOnce() -> T,
    ) -> T {
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = [
            (
                NOTIFICATION_TRANSPORT_ENV,
                std::env::var(NOTIFICATION_TRANSPORT_ENV).ok(),
            ),
            (
                NOTIFICATION_ROUTE_ENV,
                std::env::var(NOTIFICATION_ROUTE_ENV).ok(),
            ),
        ];
        for (name, value) in [
            (NOTIFICATION_TRANSPORT_ENV, transport),
            (NOTIFICATION_ROUTE_ENV, route),
        ] {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let result = operation();
        for (name, value) in previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        result
    }

    #[test]
    fn route_round_trips_through_metadata() {
        let route = NotificationRoute::new("extension", "opaque/thread 42").expect("route");
        let mut metadata = serde_json::json!({"existing": true});
        route.insert_into_metadata(&mut metadata);
        assert_eq!(NotificationRoute::from_metadata(&metadata), Some(route));
    }

    #[test]
    fn resolution_evidence_preserves_exact_metadata_fields() {
        let resolution = NotificationRouteResolution {
            schema: "homeboy/notification-route-resolution/v1".to_string(),
            classification: "route_less".to_string(),
            transport: None,
            resolver_transport: Some("example.completed".to_string()),
            missing_context: vec!["CALLER_THREAD_ID".to_string()],
        };
        let mut metadata = serde_json::json!({"existing": true});
        resolution.insert_into_metadata(&mut metadata);
        assert_eq!(
            metadata,
            serde_json::json!({
                "existing": true,
                "notification_resolution": {
                    "schema": "homeboy/notification-route-resolution/v1",
                    "classification": "route_less",
                    "resolver_transport": "example.completed",
                    "missing_context": ["CALLER_THREAD_ID"]
                }
            })
        );
    }

    /// Malformed and credential-bearing routes are rejected before they can be
    /// stored or propagated to a child environment, and the error never echoes
    /// the destination.
    #[test]
    fn invalid_routes_are_rejected_without_echoing_the_destination() {
        for (transport, destination) in [
            ("bad transport", "route"),
            ("extension", ""),
            ("extension", "line\nbreak"),
            ("extension", "token=credential"),
            ("extension", "https://example.invalid/hook?token=secret"),
            ("extension", "https://user:pass@example.invalid/hook"),
            ("extension", "channel?authorization=Bearer%20abc"),
        ] {
            assert!(
                NotificationRoute::new(transport, destination).is_err(),
                "{transport:?} {destination:?} must be rejected"
            );
        }
        let error = NotificationRoute::new("extension", "opaque\ndestination").unwrap_err();
        assert!(error.details.get("id").is_none());
        assert!(!error.details.to_string().contains("destination"));
    }

    #[test]
    fn concurrent_scopes_do_not_cross_deliver_routes() {
        let first = std::thread::spawn(|| {
            with_current(
                Some(NotificationRoute::new("extension", "first").unwrap()),
                || current().unwrap().route,
            )
        });
        let second = std::thread::spawn(|| {
            with_current(
                Some(NotificationRoute::new("extension", "second").unwrap()),
                || current().unwrap().route,
            )
        });
        assert_eq!(first.join().unwrap(), "first");
        assert_eq!(second.join().unwrap(), "second");
        assert!(current().is_none());
    }

    #[test]
    fn captured_route_is_rebound_on_a_worker_thread() {
        let observed = with_current(
            Some(NotificationRoute::new("extension", "parent-route").unwrap()),
            || {
                let propagated = capture();
                std::thread::spawn(move || propagated.bind(current))
                    .join()
                    .unwrap()
            },
        );
        assert_eq!(observed.unwrap().route, "parent-route");

        // Capturing with no bound route leaves workers unrouted.
        let propagated = capture();
        let observed = std::thread::spawn(move || propagated.bind(current))
            .join()
            .unwrap();
        assert!(observed.is_none());
    }

    /// The producer and consumer halves are the same mechanism: what
    /// `child_env` writes is exactly what a child's `from_cli_or_env` reads,
    /// and an absent route emits neither variable.
    #[test]
    fn child_env_round_trips_through_the_environment_reader() {
        assert!(child_env(None).is_empty());
        let route = NotificationRoute::new("extension", "opaque-destination").expect("route");
        let environment: std::collections::HashMap<_, _> =
            child_env(Some(&route)).into_iter().collect();
        let resolved = with_route_env(
            environment
                .get(NOTIFICATION_TRANSPORT_ENV)
                .map(String::as_str),
            environment.get(NOTIFICATION_ROUTE_ENV).map(String::as_str),
            || from_cli_or_env(None, None).unwrap(),
        );
        assert_eq!(resolved, NotificationRouteContext::Route(route));
    }

    #[test]
    fn child_env_carries_safe_resolution_evidence_without_the_route() {
        let route = NotificationRoute::new("generic.completed", "opaque-destination").unwrap();
        let mut resolution = NotificationRouteResolution::new("resolver");
        resolution.transport = Some("generic.completed".to_string());
        resolution.resolver_transport = Some("generic.completed".to_string());

        let environment =
            with_current_resolution(Some(resolution.clone()), || child_env(Some(&route)));
        let serialized = environment
            .iter()
            .find_map(|(name, value)| (*name == NOTIFICATION_RESOLUTION_ENV).then_some(value))
            .expect("resolution environment");

        assert_eq!(
            serde_json::from_str::<NotificationRouteResolution>(serialized).unwrap(),
            resolution
        );
        assert!(!serialized.contains("opaque-destination"));
    }

    #[test]
    fn propagated_resolution_requires_the_same_transport() {
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let old_resolution = std::env::var(NOTIFICATION_RESOLUTION_ENV).ok();
        let route = NotificationRoute::new("generic.completed", "opaque-destination").unwrap();
        let mut resolution = NotificationRouteResolution::new("resolver");
        resolution.transport = Some("generic.completed".to_string());
        std::env::set_var(
            NOTIFICATION_RESOLUTION_ENV,
            serde_json::to_string(&resolution).unwrap(),
        );

        assert_eq!(
            propagated_resolution_from_env(Some(&route)),
            Some(resolution)
        );
        let other_route = NotificationRoute::new("other.completed", "other-route").unwrap();
        assert!(propagated_resolution_from_env(Some(&other_route)).is_none());

        match old_resolution {
            Some(value) => std::env::set_var(NOTIFICATION_RESOLUTION_ENV, value),
            None => std::env::remove_var(NOTIFICATION_RESOLUTION_ENV),
        }
    }

    /// Explicit argv outranks the environment; a transport alone is valid
    /// context (so a caller can consult that transport's route resolver); a
    /// route without its owning transport is rejected from either source.
    #[test]
    fn cli_or_env_context_resolution() {
        let route = |transport: &str, route: &str| {
            Some(NotificationRouteContext::Route(
                NotificationRoute::new(transport, route).unwrap(),
            ))
        };
        let transport_only =
            |transport: &str| Some(NotificationRouteContext::TransportOnly(transport.into()));
        type Case<'a> = (
            (Option<&'a str>, Option<&'a str>),
            (Option<&'a str>, Option<&'a str>),
            Option<NotificationRouteContext>,
        );
        let cases: Vec<Case> = vec![
            (
                (Some("cli.transport"), Some("cli-route")),
                (Some("env.transport"), Some("env-route")),
                route("cli.transport", "cli-route"),
            ),
            (
                (None, None),
                (Some("env.transport"), Some("env-route")),
                route("env.transport", "env-route"),
            ),
            (
                (Some("chosen.transport"), None),
                (None, None),
                transport_only("chosen.transport"),
            ),
            (
                (None, None),
                (Some("env.transport"), None),
                transport_only("env.transport"),
            ),
            (
                (None, None),
                (None, None),
                Some(NotificationRouteContext::None),
            ),
            ((Some("bad transport"), None), (None, None), None),
            ((None, Some("cli-route")), (None, None), None),
            ((None, None), (None, Some("env-route")), None),
        ];
        for ((cli_transport, cli_route), (env_transport, env_route), expected) in cases {
            let resolved = with_route_env(env_transport, env_route, || {
                from_cli_or_env(cli_transport, cli_route).ok()
            });
            assert_eq!(
                resolved, expected,
                "cli=({cli_transport:?}, {cli_route:?}) env=({env_transport:?}, {env_route:?})"
            );
        }
    }
}
