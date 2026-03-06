from __future__ import annotations

from typing import Any


def render_markdown(
    lint_digest: dict[str, Any],
    test_digest: dict[str, Any],
    audit_digest: dict[str, Any],
    autofixability: dict[str, Any],
    run_url: str,
    tooling: dict[str, str],
    job_links: dict[str, str],
    results: dict[str, Any],
) -> str:
    lines: list[str] = []
    lines.append("## Failure Digest")
    lines.append("")

    lines.append("### Tooling versions")
    lines.append(f"- Homeboy CLI: **{tooling.get('homeboy_cli_version', 'unknown')}**")
    lines.append(
        "- Extension: "
        f"**{tooling.get('extension_id', 'auto')}** from "
        f"`{tooling.get('extension_source', 'auto')}`"
    )
    lines.append(
        f"- Extension revision: `{tooling.get('extension_revision', 'unknown')}`"
    )
    lines.append(
        "- Action: "
        f"`{tooling.get('action_repository', 'unknown')}@{tooling.get('action_ref', 'unknown')}`"
    )
    lines.append("")

    if "lint" in results:
        lines.append("### Lint Failure Digest")
        if lint_digest.get("lint_summary"):
            lines.append(f"- Lint summary: **{lint_digest.get('lint_summary')}**")
        if lint_digest.get("phpcs_summary"):
            lines.append(f"- PHPCS: {lint_digest.get('phpcs_summary')}")
        if lint_digest.get("phpstan_summary"):
            lines.append(f"- PHPStan: {lint_digest.get('phpstan_summary')}")
        if lint_digest.get("build_failed"):
            lines.append(f"- Build failed: {lint_digest.get('build_failed')}")
        top_violations = lint_digest.get("top_violations", []) or []
        if top_violations:
            lines.append("")
            lines.append("<details><summary>Top lint violations</summary>")
            lines.append("")
            lines.append("```text")
            for violation in top_violations[:10]:
                lines.append(str(violation))
            lines.append("```")
            lines.append("")
            lines.append("</details>")
        if (
            not lint_digest.get("lint_summary")
            and not lint_digest.get("phpcs_summary")
            and not lint_digest.get("phpstan_summary")
            and not lint_digest.get("build_failed")
            and not top_violations
        ):
            lines.append("- No structured lint details parsed from lint log.")
        build_job_url = job_links.get("Homeboy Build (Lint & Test)", run_url)
        lines.append(f"- Full lint log: {build_job_url}")
        lines.append("")

    if "test" in results:
        lines.append("### Test Failure Digest")
        lines.append(f"- Failed tests: **{test_digest.get('failed_tests_count', 0)}**")
        top_tests = test_digest.get("top_failed_tests", []) or []
        if top_tests:
            lines.append("- Top failed tests:")
            for idx, test in enumerate(top_tests[:5], start=1):
                name = test.get("name", "unknown")
                detail = test.get("detail", "")
                location = test.get("location", "")
                parts = [f"{idx}. **{name}**"]
                if detail:
                    parts.append(detail)
                if location:
                    parts.append(f"`{location}`")
                lines.append("  " + " — ".join(parts))
        else:
            lines.append("- No per-test failure details parsed from test log.")
        test_job_url = job_links.get("Homeboy Build (Lint & Test)", run_url)
        lines.append(f"- Full test log: {test_job_url}")
        lines.append("")

    if "audit" in results:
        lines.append("### Audit Failure Digest")
        severity_counts = audit_digest.get("severity_counts", {}) or {}
        if severity_counts:
            sev_text = ", ".join(f"{k}: {v}" for k, v in sorted(severity_counts.items()))
            lines.append(f"- Severity counts: **{sev_text}**")
        outliers = audit_digest.get("outliers_found")
        if isinstance(outliers, int):
            lines.append(f"- Outliers in current run: **{outliers}**")
        parsed_outliers = audit_digest.get("parsed_outlier_items")
        if isinstance(parsed_outliers, int) and parsed_outliers > 0:
            lines.append(f"- Parsed outlier entries: **{parsed_outliers}**")
        lines.append(f"- Drift increased: **{'yes' if audit_digest.get('drift_increased') else 'no'}**")

        top_findings = audit_digest.get("top_findings", []) or []
        if top_findings:
            lines.append("- Top actionable findings:")
            for idx, finding in enumerate(top_findings[:5], start=1):
                line = (
                    f"  {idx}. **{finding.get('file','unknown')}** — "
                    f"{finding.get('rule','unknown')} — {finding.get('message','')}"
                )
                lines.append(line)

            lines.append("")
            lines.append(
                f"<details><summary>All parsed audit findings ({len(top_findings)})</summary>"
            )
            lines.append("")
            max_full_findings = 300
            full_findings = top_findings
            if len(full_findings) > max_full_findings:
                full_findings = full_findings[:max_full_findings]
            for idx, finding in enumerate(full_findings, start=1):
                line = (
                    f"{idx}. **{finding.get('file','unknown')}** — "
                    f"{finding.get('rule','unknown')} — {finding.get('message','')}"
                )
                lines.append(line)
            if len(top_findings) > max_full_findings:
                lines.append("")
                lines.append(
                    f"_Truncated to {max_full_findings} findings to avoid oversized PR comments "
                    f"({len(top_findings)} total parsed)._"
                )
            lines.append("")
            lines.append("</details>")
        else:
            lines.append("- No structured audit findings parsed from audit log.")
        audit_job_url = job_links.get("Homeboy Audit", run_url)
        lines.append(f"- Full audit log: {audit_job_url}")
        lines.append("")

    lines.append("### Autofixability classification")
    lines.append(f"- Overall: **{autofixability.get('overall', 'unknown')}**")
    lines.append(
        f"- Autofix enabled: **{'yes' if autofixability.get('autofix_enabled') else 'no'}**"
    )
    lines.append(
        f"- Autofix attempted this run: **{'yes' if autofixability.get('autofix_attempted') else 'no'}**"
    )

    fixable = autofixability.get("auto_fixable_failed_commands", []) or []
    potential_fixable_failed = (
        autofixability.get("potential_auto_fixable_failed_commands", []) or []
    )
    human = autofixability.get("human_needed_failed_commands", []) or []
    if fixable:
        lines.append("- Auto-fixable failed commands:")
        for cmd in fixable:
            lines.append(f"  - `{cmd}`")
    if human:
        lines.append("- Human-needed failed commands:")
        for cmd in human:
            lines.append(f"  - `{cmd}`")
    if not fixable and not human:
        lines.append("- No failed commands to classify.")

    if potential_fixable_failed:
        lines.append("- Potentially auto-fixable failed commands (if autofix enabled):")
        for cmd in potential_fixable_failed:
            lines.append(f"  - `{cmd}`")

    if not autofixability.get("autofix_enabled"):
        potential_candidates = autofixability.get("potential_fixable_candidates", []) or []
        if potential_candidates:
            lines.append(
                "- Autofix is currently **disabled**. Commands with autofix support in this run: "
                + ", ".join(f"`{cmd}`" for cmd in potential_candidates)
            )
        else:
            lines.append(
                "- Autofix is currently **disabled** and no autofix-capable commands were detected."
            )
    lines.append("")

    lines.append("### Machine-readable artifacts")
    lines.append("- `homeboy-test-failures.json`")
    lines.append("- `homeboy-audit-summary.json`")
    lines.append("- `homeboy-autofixability.json`")

    if job_links:
        lines.append("")
        lines.append("### Failed job links")
        for name, url in sorted(job_links.items()):
            lines.append(f"- {name}: {url}")

    return "\n".join(lines)
