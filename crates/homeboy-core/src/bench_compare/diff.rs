//! Per-route base-vs-head deltas and their aggregate summary.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use super::{
    BenchCompareFinding, BenchCompareFindingDelta, BenchCompareRouteDelta,
    BenchCompareRouteObservation, BenchCompareRouteStatus, BenchCompareSideResults,
    BenchCompareSummary, BenchCompareSurfaceDelta,
};

/// Diff every route either side produced. Routes are matched by their `route`
/// identity; when a side reports one route twice, its last observation wins.
pub fn diff_routes(
    base: &BenchCompareSideResults,
    head: &BenchCompareSideResults,
) -> Vec<BenchCompareRouteDelta> {
    let base_routes = index_routes(base);
    let head_routes = index_routes(head);
    let routes: BTreeSet<&str> = base_routes
        .keys()
        .chain(head_routes.keys())
        .copied()
        .collect();

    routes
        .into_iter()
        .map(|route| diff_route(route, base_routes.get(route), head_routes.get(route)))
        .collect()
}

pub fn summarize_routes(routes: &[BenchCompareRouteDelta]) -> BenchCompareSummary {
    let mut summary = BenchCompareSummary {
        routes: routes.len(),
        ..Default::default()
    };
    for route in routes {
        match route.status {
            BenchCompareRouteStatus::Improved => summary.improved += 1,
            BenchCompareRouteStatus::Regressed => summary.regressed += 1,
            BenchCompareRouteStatus::Unchanged => summary.unchanged += 1,
            BenchCompareRouteStatus::AddedOnHead => summary.added_on_head += 1,
            BenchCompareRouteStatus::MissingOnHead => summary.missing_on_head += 1,
        }
        if route.output_changed {
            summary.output_changed += 1;
        }
        summary.new_blockers += route.blockers.new.len();
        summary.resolved_blockers += route.blockers.resolved.len();
        summary.new_findings += route.findings.new.len();
        summary.resolved_findings += route.findings.resolved.len();
    }
    summary
}

fn index_routes(
    results: &BenchCompareSideResults,
) -> BTreeMap<&str, &BenchCompareRouteObservation> {
    results
        .routes
        .iter()
        .map(|route| (route.route.as_str(), route))
        .collect()
}

fn diff_route(
    route: &str,
    base: Option<&&BenchCompareRouteObservation>,
    head: Option<&&BenchCompareRouteObservation>,
) -> BenchCompareRouteDelta {
    let base = base.copied();
    let head = head.copied();

    let content = surface_delta(
        base.map(|r| (r.content.as_deref(), r.content_sha256.as_deref())),
        head.map(|r| (r.content.as_deref(), r.content_sha256.as_deref())),
    );
    let block_markup = surface_delta(
        base.map(|r| (r.block_markup.as_deref(), r.block_markup_sha256.as_deref())),
        head.map(|r| (r.block_markup.as_deref(), r.block_markup_sha256.as_deref())),
    );
    let css = surface_delta(
        base.map(|r| (r.css.as_deref(), r.css_sha256.as_deref())),
        head.map(|r| (r.css.as_deref(), r.css_sha256.as_deref())),
    );
    let blockers = finding_delta(
        base.map(|r| r.blockers.as_slice()).unwrap_or(&[]),
        head.map(|r| r.blockers.as_slice()).unwrap_or(&[]),
    );
    let findings = finding_delta(
        base.map(|r| r.findings.as_slice()).unwrap_or(&[]),
        head.map(|r| r.findings.as_slice()).unwrap_or(&[]),
    );

    let status = match (base, head) {
        (Some(_), None) => BenchCompareRouteStatus::MissingOnHead,
        (None, _) => BenchCompareRouteStatus::AddedOnHead,
        (Some(_), Some(_)) => evaluator_status(&blockers, &findings),
    };

    BenchCompareRouteDelta {
        route: route.to_string(),
        status,
        output_changed: content.changed || block_markup.changed || css.changed,
        content,
        block_markup,
        css,
        blockers,
        findings,
    }
}

/// Blockers dominate: a route that gained more blockers than it lost has
/// regressed whatever happened to its findings. Findings decide only when the
/// blocker set is net-neutral.
fn evaluator_status(
    blockers: &BenchCompareFindingDelta,
    findings: &BenchCompareFindingDelta,
) -> BenchCompareRouteStatus {
    use std::cmp::Ordering;
    let net = |delta: &BenchCompareFindingDelta| delta.new.len().cmp(&delta.resolved.len());
    match net(blockers).then_with(|| net(findings)) {
        Ordering::Greater => BenchCompareRouteStatus::Regressed,
        Ordering::Less => BenchCompareRouteStatus::Improved,
        Ordering::Equal => BenchCompareRouteStatus::Unchanged,
    }
}

type SurfaceInput<'a> = Option<(Option<&'a str>, Option<&'a str>)>;

fn surface_delta(base: SurfaceInput<'_>, head: SurfaceInput<'_>) -> BenchCompareSurfaceDelta {
    let (base_sha256, base_bytes) = surface_identity(base);
    let (head_sha256, head_bytes) = surface_identity(head);
    BenchCompareSurfaceDelta {
        changed: base_sha256 != head_sha256,
        base_sha256,
        head_sha256,
        base_bytes,
        head_bytes,
    }
}

fn surface_identity(input: SurfaceInput<'_>) -> (Option<String>, Option<usize>) {
    match input {
        Some((Some(text), _)) => (
            Some(format!("{:x}", Sha256::digest(text.as_bytes()))),
            Some(text.len()),
        ),
        Some((None, Some(digest))) => (Some(digest.trim().to_ascii_lowercase()), None),
        _ => (None, None),
    }
}

fn finding_delta(
    base: &[BenchCompareFinding],
    head: &[BenchCompareFinding],
) -> BenchCompareFindingDelta {
    let base_keys: BTreeSet<String> = base.iter().map(BenchCompareFinding::key).collect();
    let head_keys: BTreeSet<String> = head.iter().map(BenchCompareFinding::key).collect();
    BenchCompareFindingDelta {
        base_count: base.len(),
        head_count: head.len(),
        new: head_keys.difference(&base_keys).cloned().collect(),
        resolved: base_keys.difference(&head_keys).cloned().collect(),
    }
}
