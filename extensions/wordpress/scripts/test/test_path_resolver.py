#!/usr/bin/env python3
"""Resolve a WordPress component's PHP test-environment selection.

This is the single resolver for the test-path settings a component declares in
its homeboy.json WordPress extension settings. `test-runner.sh` invokes it as a
CLI and `test-inventory.py` imports it, so glob expansion has exactly one
implementation and the suite the runner executes is the suite the inventory
enumerates.

Settings:

* `standalone_php_test_paths` -- PHP scripts that run on host PHP with no booted
  WordPress. Absent (or null) means the convention default
  `["tests/**/*-smoke.php"]`; present replaces the default, so `[]` selects none.
* `wordpress_test_paths` -- PHP smokes that need a booted WordPress. A file
  matched by both lists is a WordPress test only.
* `test_path_excludes` -- fixtures/helpers removed from both lists.

Patterns are component-relative globs matched per path segment: `*`, `?` and
`[...]` never cross `/`, and `**` matches zero or more whole directories, so
`tests/**/*-smoke.php` selects both `tests/a-smoke.php` and
`tests/x/y/b-smoke.php`. Only existing `.php` files are selected. Results are
de-duplicated and sorted.

CLI output is one `<environment>\\t<path>` line per selected file, where the
environment is `standalone-php` or `wordpress`. Invalid settings exit 2 with an
`ERROR:` line on stderr.
"""

import json
import os
import re
import sys
from pathlib import Path

DEFAULT_STANDALONE_PHP_TEST_PATHS = ["tests/**/*-smoke.php"]

# Directories a wildcard never descends into. A literal pattern segment can
# still name them explicitly.
SKIPPED_DIRECTORIES = {".git", "node_modules", "vendor"}

GLOB_CHARACTERS = re.compile(r"[*?\[]")


class TestPathError(Exception):
    """A test-path setting is malformed; the message is user-facing."""


def settings_from_environment():
    raw = os.environ.get("HOMEBOY_SETTINGS_JSON") or "{}"
    try:
        value = json.loads(raw)
    except json.JSONDecodeError:
        value = {}
    return value if isinstance(value, dict) else {}


def _declared_patterns(settings, setting, default):
    value = settings.get(setting)
    if value is None:
        return list(default)
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise TestPathError(f"{setting} must be an array of strings.")
    patterns = []
    for pattern in value:
        if not pattern:
            continue
        segments = pattern.split("/")
        if pattern.startswith("/") or ".." in segments:
            raise TestPathError(
                f"{setting} contains an invalid component-relative selector: {pattern}"
            )
        patterns.append(pattern)
    return patterns


def _segment_regex(segment):
    out = []
    index = 0
    while index < len(segment):
        char = segment[index]
        if char == "*":
            out.append("[^/]*")
        elif char == "?":
            out.append("[^/]")
        elif char == "[":
            end = segment.find("]", index + 1)
            if end == -1 or end == index + 1:
                out.append(re.escape(char))
            else:
                body = segment[index + 1:end]
                if body[:1] in ("!", "^"):
                    body = "^" + body[1:]
                out.append("[" + body.replace("\\", "\\\\").replace("/", "") + "]")
                index = end
        else:
            out.append(re.escape(char))
        index += 1
    return "".join(out)


def compile_pattern(pattern):
    """Translate a component-relative glob into an anchored regex."""
    segments = [segment for segment in pattern.split("/") if segment not in ("", ".")]
    parts = []
    for position, segment in enumerate(segments):
        last = position == len(segments) - 1
        if segment == "**":
            parts.append(".*" if last else "(?:[^/]+/)*")
        else:
            parts.append(_segment_regex(segment) + ("" if last else "/"))
    return re.compile("".join(parts) + r"\Z")


def _candidate_files(project, pattern):
    """Yield component-relative `.php` paths that could match `pattern`."""
    segments = [segment for segment in pattern.split("/") if segment not in ("", ".")]
    literal = []
    for segment in segments:
        if GLOB_CHARACTERS.search(segment):
            break
        literal.append(segment)
    if len(literal) == len(segments):
        relative = "/".join(segments)
        if relative.endswith(".php") and (project / relative).is_file():
            yield relative
        return
    base = project.joinpath(*literal)
    if not base.is_dir():
        return
    for directory, subdirectories, files in os.walk(base):
        subdirectories[:] = sorted(name for name in subdirectories if name not in SKIPPED_DIRECTORIES)
        for name in files:
            if not name.endswith(".php"):
                continue
            path = Path(directory) / name
            if path.is_file():
                yield path.relative_to(project).as_posix()


def _expand(project, patterns):
    selected = set()
    for pattern in patterns:
        regex = compile_pattern(pattern)
        for relative in _candidate_files(project, pattern):
            if regex.match(relative):
                selected.add(relative)
    return selected


def _matches_any(relative, patterns):
    return any(compile_pattern(pattern).match(relative) for pattern in patterns)


def resolve(project, settings=None):
    """Return `{"standalone-php": [...], "wordpress": [...]}` for a component."""
    if settings is None:
        settings = settings_from_environment()
    project = Path(project).resolve()
    standalone_patterns = _declared_patterns(
        settings, "standalone_php_test_paths", DEFAULT_STANDALONE_PHP_TEST_PATHS
    )
    wordpress_patterns = _declared_patterns(settings, "wordpress_test_paths", [])
    exclude_patterns = _declared_patterns(settings, "test_path_excludes", [])

    wordpress = {
        path for path in _expand(project, wordpress_patterns)
        if not _matches_any(path, exclude_patterns)
    }
    standalone = {
        path for path in _expand(project, standalone_patterns)
        if path not in wordpress and not _matches_any(path, exclude_patterns)
    }
    return {"standalone-php": sorted(standalone), "wordpress": sorted(wordpress)}


def main(argv):
    if len(argv) != 2:
        print("usage: test_path_resolver.py <component-path>", file=sys.stderr)
        return 2
    try:
        resolved = resolve(argv[1])
    except TestPathError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    for environment in ("standalone-php", "wordpress"):
        for path in resolved[environment]:
            print(f"{environment}\t{path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
