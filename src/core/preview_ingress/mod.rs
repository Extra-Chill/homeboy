//! Homeboy preview ingress: route persistence, install planning, and the
//! loopback HTTP server that proxies preview traffic to upstream origins or
//! reverse-channel preview clients.
//!
//! The implementation is split into focused submodules:
//! - [`types`]: shared data structures and wire types.
//! - [`routes`]: route persistence, status, and lifecycle classification.
//! - [`install`]: operator install/status plan rendering.
//! - [`serve`]: the TCP server loop, client API, and request proxying.
//! - [`http`]: low-level HTTP response writing helpers.

mod http;
mod install;
mod routes;
mod serve;
mod types;

pub use install::{render_install_plan, render_install_status_plan};
pub use routes::{list_routes, register_route, remove_route, status, status_for_host};
pub use serve::serve;
pub use types::{
    PreviewIngressFailure, PreviewIngressInstallCheck, PreviewIngressInstallCheckStatus,
    PreviewIngressInstallOptions, PreviewIngressInstallPlan, PreviewIngressInstallStatusPlan,
    PreviewIngressRoute, PreviewIngressRouteLifecycle, PreviewIngressRouteStatus,
    PreviewIngressServeSpec, PreviewIngressStatus, PreviewIngressWrite,
};

#[cfg(test)]
pub(crate) use serve::serve_listener;
