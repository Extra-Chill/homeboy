use std::fs;
use std::sync::Arc;

use crate::core::error::{Error, Result};
use crate::core::paths;

use super::types::{
    PreviewClientSessions, PreviewIngressFailure, PreviewIngressRoute, PreviewIngressRouteLifecycle,
    PreviewIngressRouteStatus, PreviewIngressStatus,
};

pub fn register_route(route: PreviewIngressRoute) -> Result<PreviewIngressRoute> {
    validate_route(&route)?;
    let path = paths::preview_ingress_route_file(&route.session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::internal_io(e.to_string(), Some(parent.display().to_string())))?;
    }
    let data = serde_json::to_string_pretty(&route)
        .map_err(|e| Error::internal_json(e.to_string(), Some(route.session_id.clone())))?;
    fs::write(&path, data)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    load_route(&route.session_id)
}

pub fn remove_route(session_id: &str) -> Result<()> {
    let path = paths::preview_ingress_route_file(session_id)?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    }
    Ok(())
}

pub(crate) fn load_route(session_id: &str) -> Result<PreviewIngressRoute> {
    let path = paths::preview_ingress_route_file(session_id)?;
    let data = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&data)
        .map_err(|e| Error::internal_json(e.to_string(), Some(path.display().to_string())))
}

pub fn list_routes() -> Result<Vec<PreviewIngressRoute>> {
    let dir = paths::preview_ingress_routes_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut routes: Vec<PreviewIngressRoute> = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some(dir.display().to_string())))?
    {
        let entry = entry.map_err(|e| Error::internal_io(e.to_string(), None))?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let data = fs::read_to_string(entry.path()).map_err(|e| {
            Error::internal_io(e.to_string(), Some(entry.path().display().to_string()))
        })?;
        routes.push(serde_json::from_str(&data).map_err(|e| {
            Error::internal_json(e.to_string(), Some(entry.path().display().to_string()))
        })?);
    }
    routes.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Ok(routes)
}

pub fn status(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
) -> Result<PreviewIngressStatus> {
    status_for_host(bind, domain, public_host_pattern, None)
}

pub fn status_for_host(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
    host: Option<String>,
) -> Result<PreviewIngressStatus> {
    let inspected_host = host.map(|host| normalize_public_host(&host));
    let inspected_state = inspected_host.as_ref().map(|host| {
        classify_route_host_state(host).unwrap_or_else(|| "missing_session".to_string())
    });
    status_with_failures(
        bind,
        domain,
        public_host_pattern,
        Vec::new(),
        inspected_host,
        inspected_state,
    )
}

pub(crate) fn status_with_failures(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
    recent_failures: Vec<PreviewIngressFailure>,
    inspected_host: Option<String>,
    inspected_state: Option<String>,
) -> Result<PreviewIngressStatus> {
    Ok(PreviewIngressStatus {
        bind,
        domain,
        public_host_pattern,
        routes: list_routes()?
            .into_iter()
            .map(|route| PreviewIngressRouteStatus {
                lifecycle: classify_route(&route),
                route,
            })
            .collect(),
        recent_failures,
        inspected_host,
        inspected_state,
    })
}

pub(crate) fn route_for_host(host: &str) -> Result<Option<PreviewIngressRoute>> {
    Ok(list_routes()?.into_iter().find(|route| {
        route.public_host.eq_ignore_ascii_case(host)
            || route
                .public_host
                .split(':')
                .next()
                .is_some_and(|public_host| public_host.eq_ignore_ascii_case(host))
    }))
}

pub(crate) fn classify_route(route: &PreviewIngressRoute) -> PreviewIngressRouteLifecycle {
    if !route.active {
        return PreviewIngressRouteLifecycle::Disconnected;
    }
    if route
        .expires_at
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .is_some_and(|expires_at| chrono::Utc::now() > expires_at)
    {
        return PreviewIngressRouteLifecycle::Expired;
    }
    PreviewIngressRouteLifecycle::Active
}

pub(crate) fn normalize_public_host(host: &str) -> String {
    host.trim()
        .trim_end_matches('.')
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub(crate) fn classify_runtime_host_state(
    public_host: &str,
    sessions: &Arc<PreviewClientSessions>,
    recent_failures: &[PreviewIngressFailure],
) -> Option<String> {
    let sessions_guard = sessions
        .sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(session) = sessions_guard.get(public_host) {
        return Some(
            if session.active {
                "registered"
            } else {
                "disconnected"
            }
            .to_string(),
        );
    }
    drop(sessions_guard);
    if recent_failures.iter().rev().any(|failure| {
        normalize_public_host(&failure.host) == public_host
            && failure.classification == "auth_failed_recently"
    }) {
        return Some("auth_failed_recently".to_string());
    }
    route_for_host(public_host)
        .ok()
        .flatten()
        .map(|route| route_state_label(&route))
}

pub(crate) fn classify_route_host_state(public_host: &str) -> Option<String> {
    route_for_host(public_host)
        .ok()
        .flatten()
        .map(|route| route_state_label(&route))
}

fn route_state_label(route: &PreviewIngressRoute) -> String {
    match classify_route(route) {
        PreviewIngressRouteLifecycle::Active => "registered".to_string(),
        PreviewIngressRouteLifecycle::Expired => "missing_session".to_string(),
        PreviewIngressRouteLifecycle::Disconnected => "disconnected".to_string(),
    }
}

fn validate_route(route: &PreviewIngressRoute) -> Result<()> {
    if route.session_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "session_id",
            "preview ingress session ID is required",
            None,
            None,
        ));
    }
    if route.public_host.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview ingress public host is required",
            Some(route.session_id.clone()),
            None,
        ));
    }
    if !(route.upstream_origin.starts_with("http://")
        || route.upstream_origin.starts_with("https://"))
    {
        return Err(Error::validation_invalid_argument(
            "upstream_origin",
            "upstream origin must be an http:// or https:// URL",
            Some(route.session_id.clone()),
            None,
        ));
    }
    if route
        .expires_at
        .as_deref()
        .is_some_and(|expires_at| parse_rfc3339_utc(expires_at).is_none())
    {
        return Err(Error::validation_invalid_argument(
            "expires_at",
            "preview ingress expiry must be a valid RFC3339 timestamp",
            Some(route.session_id.clone()),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.with_timezone(&chrono::Utc))
}
