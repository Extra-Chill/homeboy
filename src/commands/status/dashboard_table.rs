//! Human-readable dashboard table rendering for `homeboy status <project>`.
//!
//! Extracted from the `status` command entry to keep that file a thin command
//! surface. This module owns the terminal-only table layout (column widths,
//! header, separator, rows) and the compact upstream ahead/behind formatter.
//! All output goes to stderr and is gated on an interactive terminal, so JSON
//! consumers are unaffected.

use super::ProjectComponentDashboardStatus;
use super::ProjectStatusRow;

/// Log a human-readable table to stderr.
pub(super) fn log_dashboard_table(rows: &[ProjectStatusRow]) {
    if rows.is_empty() || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }

    // Calculate column widths
    let widths = DashboardColumnWidths {
        id: rows
            .iter()
            .map(|r| r.component_id.len())
            .max()
            .unwrap_or(9)
            .max(9),
        local: rows
            .iter()
            .map(|r| r.local_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(5)
            .max(5),
        remote: rows
            .iter()
            .map(|r| r.remote_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(6)
            .max(6),
        origin: rows
            .iter()
            .map(|r| r.origin_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(6)
            .max(6),
    };

    log_dashboard_header(&widths);
    log_dashboard_separator(&widths);

    for row in rows {
        let local = row.local_version.as_deref().unwrap_or("-");
        let remote = row.remote_version.as_deref().unwrap_or("-");
        let origin = row.origin_version.as_deref().unwrap_or("-");
        let upstream = format_upstream(&row.ahead_upstream, &row.behind_upstream);
        let status_icon = match &row.status {
            ProjectComponentDashboardStatus::Current => "✅ current",
            ProjectComponentDashboardStatus::PinnedCurrent => "📌 pinned current",
            ProjectComponentDashboardStatus::Outdated => "⚠️  outdated",
            ProjectComponentDashboardStatus::NeedsRelease => "🔶 needs release",
            ProjectComponentDashboardStatus::DocsOnly => "📝 docs only",
            ProjectComponentDashboardStatus::Uncommitted => "🔴 uncommitted",
            ProjectComponentDashboardStatus::BehindUpstream => "⬇️  behind upstream",
            ProjectComponentDashboardStatus::Unknown => "❓ unknown",
        };

        eprintln!(
            "{:<id_w$}  {:<local_w$}  {:<remote_w$}  {:<origin_w$}  {:>10}  {:>8}  {}",
            row.component_id,
            local,
            remote,
            origin,
            row.unreleased_commits,
            upstream,
            status_icon,
            id_w = widths.id,
            local_w = widths.local,
            remote_w = widths.remote,
            origin_w = widths.origin,
        );
    }
}

struct DashboardColumnWidths {
    id: usize,
    local: usize,
    remote: usize,
    origin: usize,
}

fn log_dashboard_header(widths: &DashboardColumnWidths) {
    eprintln!(
        "{:<id_w$}  {:<local_w$}  {:<remote_w$}  {:<origin_w$}  {:>10}  {:>8}  Status",
        "Component",
        "Local",
        "Remote",
        "Origin",
        "Unreleased",
        "Upstream",
        id_w = widths.id,
        local_w = widths.local,
        remote_w = widths.remote,
        origin_w = widths.origin,
    );
}

fn log_dashboard_separator(widths: &DashboardColumnWidths) {
    eprintln!(
        "{:-<id_w$}  {:-<local_w$}  {:-<remote_w$}  {:-<origin_w$}  {:->10}  {:->8}  {:-<10}",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        id_w = widths.id,
        local_w = widths.local,
        remote_w = widths.remote,
        origin_w = widths.origin,
    );
}

/// Format upstream ahead/behind as a compact string like "↓3" or "↑1↓2" or "=".
fn format_upstream(ahead: &Option<u32>, behind: &Option<u32>) -> String {
    match (ahead, behind) {
        (Some(a), Some(b)) if *a > 0 && *b > 0 => format!("↑{}↓{}", a, b),
        (Some(a), Some(_)) if *a > 0 => format!("↑{}", a),
        (Some(_), Some(b)) if *b > 0 => format!("↓{}", b),
        (None, Some(b)) if *b > 0 => format!("↓{}", b),
        (Some(a), None) if *a > 0 => format!("↑{}", a),
        (Some(0), Some(0)) | (None, Some(0)) | (Some(0), None) => "=".to_string(),
        _ => "-".to_string(),
    }
}
