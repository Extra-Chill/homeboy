from __future__ import annotations

from typing import Any


def _format_audit_finding(finding: dict[str, Any]) -> str:
    file_value = str(finding.get("file", "unknown"))
    rule_value = str(finding.get("rule", "unknown"))
    message_value = str(finding.get("message", ""))
    parts = [f"**{file_value}**", rule_value]
    if message_value:
        parts.append(message_value)
    return " — ".join(parts)


def _format_new_audit_finding(finding: dict[str, Any]) -> str:
    context = str(finding.get("context", "unknown"))
    message = str(finding.get("message", ""))
    fingerprint = str(finding.get("fingerprint", ""))
    line = f"**{context}**"
    if message:
        line += f" — {message}"
    if fingerprint:
        line += f" (`{fingerprint}`)"
    return line


def _status_summary(results: dict[str, Any], audit_digest: dict[str, Any]) -> str:
    parts: list[str] = []
    ordered_commands = [cmd for cmd in ["lint", "test", "audit", "refactor"] if cmd in results]
    ordered_commands.extend(cmd for cmd in results if cmd not in ordered_commands)
    for command in ordered_commands:
        status = str(results.get(command, "unknown"))
        icon = ":white_check_mark:" if status == "pass" else ":x:"
        label = command
        if command == "audit":
            new_count = audit_digest.get("new_findings_count", 0)
            if isinstance(new_count, int) and new_count > 0:
                label += f" ({new_count} new)"
        parts.append(f"{icon} {label}")
    return " · ".join(parts)


def _append_details_block(lines: list[str], summary: str, block_lines: list[str]) -> None:
    content = [str(line) for line in block_lines if str(line) != ""]
    if not content:
        return
    lines.append("")
    lines.append(f"<details><summary>{summary}</summary>")
    lines.append("")
    lines.append("```text")
    lines.extend(content)
    lines.append("```")
    lines.append("")
    lines.append("</details>")


def _summarize_test_failure(test: dict[str, Any], idx: int) -> str:
    name = str(test.get("name", "unknown"))
    detail = str(test.get("detail", "")).strip()
    location = str(test.get("location", "")).strip()
    parts = [f"{idx}. {name}"]
    if detail:
        parts.append(detail)
    if location:
        parts.append(location)
    return " — ".join(parts)


def _resolve_job_link(job_links: dict[str, str], run_url: str, *candidates: str) -> str:
    for candidate in candidates:
        if candidate in job_links:
            return job_links[candidate]
    return run_url


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
    summary = _status_summary(results, audit_digest)
    if summary:
        lines.append("")
        lines.append(f"**TL;DR:** {summary}")
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
        if lint_digest.get("error_code"):
            lines.append(f"- Error code: `{lint_digest.get('error_code')}`")
        if lint_digest.get("error_message"):
            lines.append(f"- Error message: {lint_digest.get('error_message')}")
        if lint_digest.get("error_field"):
            lines.append(f"- Error field: `{lint_digest.get('error_field')}`")
        if lint_digest.get("error_hint"):
            lines.append(f"- Hint: {lint_digest.get('error_hint')}")

        top_violations = lint_digest.get("top_violations", []) or []
        if top_violations:
            _append_details_block(lines, "Top lint violations", [str(v) for v in top_violations[:10]])

        raw_excerpt = [str(line) for line in (lint_digest.get("raw_excerpt", []) or [])]
        if raw_excerpt:
            _append_details_block(lines, "Lint failure details", raw_excerpt)

        if (
            not lint_digest.get("lint_summary")
            and not lint_digest.get("phpcs_summary")
            and not lint_digest.get("phpstan_summary")
            and not lint_digest.get("build_failed")
            and not lint_digest.get("error_code")
            and not lint_digest.get("error_message")
            and not top_violations
            and not raw_excerpt
        ):
            lines.append("- No structured lint details available.")
        build_job_url = _resolve_job_link(
            job_links,
            run_url,
            "Lint",
            "Homeboy Lint",
            "Homeboy Build (Lint & Test)",
        )
        lines.append(f"- Full lint log: {build_job_url}")
        lines.append("")

    if "test" in results:
        lines.append("### Test Failure Digest")
        failed_count = int(test_digest.get("failed_tests_count", 0) or 0)
        if failed_count > 0:
            lines.append(f"- Failed tests: **{failed_count}**")
        else:
            lines.append("- Test command failed, but structured output reported **0 failed test cases**.")
            status = str(test_digest.get("status") or "").strip()
            exit_code = test_digest.get("exit_code")
            if status or exit_code is not None:
                status_label = status or "unknown"
                exit_label = "unknown" if exit_code is None else str(exit_code)
                lines.append(f"- Runner status: `{status_label}` (exit code `{exit_label}`)")
            failure_message = str(test_digest.get("failure_message") or "").strip()
            if failure_message:
                lines.append(f"- Failure: {failure_message}")
            lines.append("- Interpret this as a runner/tooling failure, not failed test assertions.")
        top_tests = test_digest.get("top_failed_tests", []) or []
        if top_tests:
            _append_details_block(
                lines,
                f"Failed test details ({min(len(top_tests), 10)} shown)",
                [_summarize_test_failure(test, idx) for idx, test in enumerate(top_tests[:10], start=1)],
            )
        elif failed_count > 0:
            lines.append("- No structured test failure details available.")

        raw_excerpt = [str(line) for line in (test_digest.get("raw_excerpt", []) or [])]
        if raw_excerpt:
            _append_details_block(lines, "Raw test failure excerpt", raw_excerpt)

        test_job_url = _resolve_job_link(
            job_links,
            run_url,
            "Test",
            "Homeboy Test",
            "Homeboy Build (Lint & Test)",
        )
        lines.append(f"- Full test log: {test_job_url}")
        lines.append("")

    if "audit" in results:
        lines.append("### Audit Failure Digest")
        new_findings = audit_digest.get("new_findings", []) or []
        new_findings_count = audit_digest.get("new_findings_count", 0)
        if isinstance(new_findings_count, int) and new_findings_count > 0:
            lines.append(f":x: **{new_findings_count} new finding(s) on this PR:**")
            lines.append("")
            for idx, finding in enumerate(new_findings[:5], start=1):
                lines.append(f"{idx}. {_format_new_audit_finding(finding)}")
            if new_findings_count > 5:
                lines.append(f"... and {new_findings_count - 5} more")
            lines.append("")
            lines.append("_Full audit state below._")

        alignment_score = audit_digest.get("alignment_score")
        if isinstance(alignment_score, (int, float)):
            lines.append(f"- Alignment score: **{alignment_score:.3f}**")
        severity_counts = audit_digest.get("severity_counts", {}) or {}
        if severity_counts:
            known_counts = {k: v for k, v in severity_counts.items() if str(k).lower() != "unknown"}
            if known_counts:
                sev_text = ", ".join(f"{k}: {v}" for k, v in sorted(known_counts.items()))
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
            max_full_findings = 300
            full_findings = top_findings[:max_full_findings]
            detail_lines = [
                f"{idx}. {_format_audit_finding(finding)}"
                for idx, finding in enumerate(full_findings, start=1)
            ]
            if len(top_findings) > max_full_findings:
                detail_lines.append("")
                detail_lines.append(
                    f"_Truncated to {max_full_findings} findings to avoid oversized PR comments ({len(top_findings)} total parsed)._"
                )
            if len(top_findings) <= 5:
                lines.append("- Actionable findings:")
                for line in detail_lines:
                    lines.append(f"  {line}")
            else:
                _append_details_block(lines, f"Audit findings ({len(top_findings)})", detail_lines)
        else:
            lines.append("- No structured audit findings available.")

        raw_excerpt = [str(line) for line in (audit_digest.get("raw_excerpt", []) or [])]
        if raw_excerpt:
            _append_details_block(lines, "Raw audit failure excerpt", raw_excerpt)

        audit_job_url = _resolve_job_link(job_links, run_url, "Homeboy Audit", "Audit")
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
        failed_commands = autofixability.get("failed_commands", []) or []
        if failed_commands:
            lines.append("- Failed commands:")
            for cmd in failed_commands:
                lines.append(f"  - `{cmd}`")
        else:
            lines.append("- No failed commands to classify.")

    if potential_fixable_failed:
        lines.append("- Failed commands with available automated fixes:")
        for cmd in potential_fixable_failed:
            lines.append(f"  - `{cmd}`")

    if not autofixability.get("autofix_enabled"):
        potential_candidates = autofixability.get("potential_fixable_candidates", []) or []
        if potential_candidates:
            lines.append(
                "- Automated fixes are **disabled for this step**. Commands with available fix support in this run: "
                + ", ".join(f"`{cmd}`" for cmd in potential_candidates)
            )
        else:
            lines.append(
                "- Automated fixes are **disabled for this step** and no fix-capable commands were detected."
            )
    lines.append("")

    lines.append("### Machine-readable artifacts")
    lines.append("- `{command}.json` — structured output per command (from `homeboy --output`)")

    if job_links:
        lines.append("")
        lines.append("### Failed job links")
        for name, url in sorted(job_links.items()):
            lines.append(f"- {name}: {url}")

    return "\n".join(lines)
