#!/usr/bin/env python3
"""Read declared PHP floors without loading any PHP source."""
import json
from itertools import islice
import re
import sys
from pathlib import Path


def header_value(path, key):
    try:
        with path.open(encoding="utf-8", errors="ignore") as handle:
            for line in islice(handle, 100):
                match = re.match(r"\s*(?:/\*+|\*|//|#)?\s*" + re.escape(key) + r":\s*(.*?)\s*(?:\*/)?$", line)
                if match and match[1]:
                    return match[1]
    except OSError:
        pass
    return None


def version(value):
    match = re.fullmatch(r"(\d+)\.(\d+)(?:\.\d+)?", value or "")
    return tuple(map(int, match.groups())) if match else None


def composer_floor(constraint):
    """Minimum major.minor across alternatives; ignore upper bounds.

    Unknown/unbounded alternatives have no usable floor (do not guess).
    Patch constraints intentionally collapse to major.minor for runtime setup.
    """
    if not isinstance(constraint, str):
        return None
    alternatives = []
    for branch in re.split(r"\|\|?", constraint):
        # Composer hyphen ranges include an upper endpoint, not a second floor.
        branch = re.sub(r"(\d+(?:\.\d+){1,2})\s+-\s+(\d+(?:\.\d+){1,2})", r">=\1 <=\2", branch)
        bounds = []
        for match in re.finditer(r"(?<![\w.])(?P<op>>=|<=|>|<|\^|~|=)?\s*(?P<v>\d+\.\d+)(?:\.(?:\d+|\*|x))?", branch):
            if match['op'] not in ('<', '<='):
                bounds.append(version(match['v']))
        if not bounds:
            return None
        alternatives.append(max(bounds))
    floor = min(alternatives)
    return '.'.join(map(str, floor))


def declared_floor(root):
    root = Path(root)
    candidates = [root / 'style.css'] + sorted(root.glob('*.php'))
    for candidate in candidates:
        kind = 'Theme Name' if candidate.name == 'style.css' else 'Plugin Name'
        if header_value(candidate, kind):
            floor = header_value(candidate, 'Requires PHP')
            if version(floor):
                return floor
    try:
        data = json.loads((root / 'composer.json').read_text())
        return composer_floor(data.get('require', {}).get('php'))
    except (OSError, ValueError, AttributeError):
        return None


def local_dependency_paths(root, seen=None):
    """Detect only explicit local checkouts; never clone during env detection."""
    root = Path(root).resolve()
    seen = set() if seen is None else seen
    if root in seen:
        return
    seen.add(root)
    try:
        data = json.loads((root / 'homeboy.json').read_text())
        settings = data.get('extensions', {}).get('wordpress', {}).get('settings', {})
        entries = settings.get('validation_dependencies', settings.get('depends_on', []))
    except (OSError, ValueError, AttributeError):
        return
    if isinstance(entries, str):
        entries = entries.split(',')
    if not isinstance(entries, list):
        return
    for entry in entries:
        package = ''
        if isinstance(entry, dict):
            package = entry.get('package_path', entry.get('subdir', ''))
            entry = entry.get('dependency', entry.get('path', entry.get('local_path', '')))
        if not isinstance(entry, str) or not entry.strip():
            continue
        path = (root / entry.strip() / package).resolve()
        if path.is_dir() and path not in seen:
            yield path
            yield from local_dependency_paths(path, seen)


def check_dependencies(running, paths):
    failed = False
    for path in paths:
        path = path.strip()
        if not path:
            continue
        floor = declared_floor(path)
        if floor and version(running) < version(floor):
            print(f"Error: validation dependency {Path(path).name} ({path}) requires PHP >= {floor}, "
                  f"but PHPStan is running PHP {running}; set "
                  "extensions.wordpress.settings.wordpress_runtime_php_version "
                  f"to {floor} or newer and install/use that PHP runtime.", file=sys.stderr)
            failed = True
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(check_dependencies(sys.argv[1], sys.stdin))
