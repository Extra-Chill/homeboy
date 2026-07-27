//! Transport-neutral, run-scoped notification routing.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};

use homeboy_error::{Error, Result};

pub const NOTIFICATION_ROUTE_METADATA_KEY: &str = "notification_route";
pub const NOTIFICATION_TRANSPORT_ENV: &str = "HOMEBOY_NOTIFICATION_TRANSPORT";
pub const NOTIFICATION_ROUTE_ENV: &str = "HOMEBOY_NOTIFICATION_ROUTE";

thread_local! {
    static CURRENT_NOTIFICATION_ROUTE: RefCell<Option<NotificationRoute>> = const { RefCell::new(None) };
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
        let valid_transport = !self.transport.is_empty()
            && self.transport.len() <= 64
            && self
                .transport
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
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
                Some(self.route.clone()),
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

/// Resolve generic caller context once for a process. Explicit CLI values win;
/// environment context is considered only when neither CLI value was supplied.
pub fn from_cli_or_env(
    cli_transport: Option<&str>,
    cli_route: Option<&str>,
) -> Result<Option<NotificationRoute>> {
    if let (Some(transport), Some(route)) = (cli_transport, cli_route) {
        return NotificationRoute::new(transport, route).map(Some);
    }
    if cli_transport.is_some() || cli_route.is_some() {
        return Err(Error::validation_invalid_argument(
            "notification_route",
            "--notification-transport and --notification-route must be supplied together",
            None,
            None,
        ));
    }
    match (
        std::env::var(NOTIFICATION_TRANSPORT_ENV).ok(),
        std::env::var(NOTIFICATION_ROUTE_ENV).ok(),
    ) {
        (None, None) => Ok(None),
        (Some(transport), Some(route)) => NotificationRoute::new(transport, route).map(Some),
        _ => Err(Error::validation_invalid_argument(
            "notification_route",
            format!(
                "{NOTIFICATION_TRANSPORT_ENV} and {NOTIFICATION_ROUTE_ENV} must be set together"
            ),
            None,
            None,
        )),
    }
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

    #[test]
    fn route_round_trips_through_metadata() {
        let route = NotificationRoute::new("extension", "opaque/thread 42").expect("route");
        let mut metadata = serde_json::json!({"existing": true});
        route.insert_into_metadata(&mut metadata);
        assert_eq!(NotificationRoute::from_metadata(&metadata), Some(route));
    }

    #[test]
    fn malformed_route_is_rejected() {
        assert!(NotificationRoute::new("bad transport", "route").is_err());
        assert!(NotificationRoute::new("extension", "").is_err());
        assert!(NotificationRoute::new("extension", "line\nbreak").is_err());
        assert!(NotificationRoute::new("extension", "token=credential").is_err());
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
    }

    #[test]
    fn capture_without_a_bound_route_leaves_workers_unrouted() {
        let propagated = capture();
        assert!(propagated.route().is_none());
        let observed = std::thread::spawn(move || propagated.bind(current))
            .join()
            .unwrap();
        assert!(observed.is_none());
    }

    #[test]
    fn cli_context_wins_over_environment_context() {
        let _lock = env_lock().lock().unwrap();
        let old_transport = std::env::var(NOTIFICATION_TRANSPORT_ENV).ok();
        let old_route = std::env::var(NOTIFICATION_ROUTE_ENV).ok();
        std::env::set_var(NOTIFICATION_TRANSPORT_ENV, "env.transport");
        std::env::set_var(NOTIFICATION_ROUTE_ENV, "env-route");
        let route = from_cli_or_env(Some("cli.transport"), Some("cli-route")).unwrap();
        assert_eq!(route.unwrap().transport, "cli.transport");
        match old_transport {
            Some(value) => std::env::set_var(NOTIFICATION_TRANSPORT_ENV, value),
            None => std::env::remove_var(NOTIFICATION_TRANSPORT_ENV),
        }
        match old_route {
            Some(value) => std::env::set_var(NOTIFICATION_ROUTE_ENV, value),
            None => std::env::remove_var(NOTIFICATION_ROUTE_ENV),
        }
    }

    #[test]
    fn environment_context_is_used_without_cli_context() {
        let _lock = env_lock().lock().unwrap();
        let old_transport = std::env::var(NOTIFICATION_TRANSPORT_ENV).ok();
        let old_route = std::env::var(NOTIFICATION_ROUTE_ENV).ok();
        std::env::set_var(NOTIFICATION_TRANSPORT_ENV, "env.transport");
        std::env::set_var(NOTIFICATION_ROUTE_ENV, "env-route");
        let route = from_cli_or_env(None, None).unwrap().unwrap();
        assert_eq!(route.transport, "env.transport");
        assert_eq!(route.route, "env-route");
        match old_transport {
            Some(value) => std::env::set_var(NOTIFICATION_TRANSPORT_ENV, value),
            None => std::env::remove_var(NOTIFICATION_TRANSPORT_ENV),
        }
        match old_route {
            Some(value) => std::env::set_var(NOTIFICATION_ROUTE_ENV, value),
            None => std::env::remove_var(NOTIFICATION_ROUTE_ENV),
        }
    }
}
