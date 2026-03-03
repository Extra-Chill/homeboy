#!/usr/bin/env python3
"""Find files related to a set of changed files by tracing symbol references.

Usage:
    find-related-files.py <workspace_dir> <changed_files_path>

Reads the list of changed files, extracts exported symbols (class names,
function names, trait/interface names, struct/enum names) from each file,
then searches the codebase for files that reference those symbols.

Outputs one related file path per line (repo-relative). Changed files
themselves are NOT included in the output — only their dependents.

Strategy:
    1. Read each changed file and extract symbol definitions via regex
    2. For each symbol, grep the workspace for files referencing it
    3. Deduplicate and output the union of all referencing files

Supported languages (by file extension):
    - PHP (.php): class, interface, trait, function, const
    - Rust (.rs): pub fn, pub struct, pub enum, pub trait, pub type, pub const
    - Python (.py): class, def (top-level only)
    - JavaScript/TypeScript (.js, .ts, .jsx, .tsx): export class, export function,
      export const, export default
"""

import os
import re
import subprocess
import sys

# Symbol extraction patterns per language
PATTERNS = {
    '.php': [
        re.compile(r'^\s*(?:abstract\s+|final\s+)?class\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*interface\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*trait\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*(?:public\s+|protected\s+|private\s+)?(?:static\s+)?function\s+(\w+)', re.MULTILINE),
    ],
    '.rs': [
        re.compile(r'^\s*pub\s+(?:async\s+)?fn\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*pub\s+struct\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*pub\s+enum\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*pub\s+trait\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*pub\s+type\s+(\w+)', re.MULTILINE),
        re.compile(r'^\s*pub\s+const\s+(\w+)', re.MULTILINE),
    ],
    '.py': [
        re.compile(r'^class\s+(\w+)', re.MULTILINE),
        re.compile(r'^def\s+(\w+)', re.MULTILINE),
    ],
    '.js': [
        re.compile(r'export\s+(?:default\s+)?class\s+(\w+)', re.MULTILINE),
        re.compile(r'export\s+(?:default\s+)?function\s+(\w+)', re.MULTILINE),
        re.compile(r'export\s+(?:const|let|var)\s+(\w+)', re.MULTILINE),
    ],
}
# Aliases: same patterns for related extensions
PATTERNS['.ts'] = PATTERNS['.js']
PATTERNS['.jsx'] = PATTERNS['.js']
PATTERNS['.tsx'] = PATTERNS['.js']

# Symbols too common to be useful as search terms
NOISE_SYMBOLS = frozenset({
    # PHP lifecycle/magic
    '__construct', '__destruct', '__get', '__set', '__call', '__toString',
    '__invoke', '__clone', '__sleep', '__wakeup', '__serialize', '__unserialize',
    # Common method names
    'get', 'set', 'run', 'init', 'load', 'save', 'delete', 'update', 'create',
    'register', 'render', 'handle', 'process', 'execute', 'validate', 'parse',
    'format', 'build', 'setup', 'teardown', 'reset', 'clear', 'close', 'open',
    'read', 'write', 'start', 'stop', 'test', 'main', 'new', 'from', 'into',
    # Rust common
    'default', 'fmt', 'clone', 'drop', 'eq', 'hash', 'cmp', 'partial_cmp',
    'deref', 'as_ref', 'as_mut', 'try_from', 'try_into',
    # Python common
    '__init__', '__repr__', '__str__', '__eq__', '__hash__', '__len__',
    # Too short
    'a', 'b', 'c', 'e', 'f', 'i', 'k', 'n', 'p', 'r', 's', 't', 'v', 'x',
    'id', 'ok', 'to', 'do', 'is', 'on', 'of', 'up',
})

# Minimum symbol length to avoid false positives
MIN_SYMBOL_LENGTH = 3


def extract_symbols(filepath: str) -> set[str]:
    """Extract exported symbol names from a source file."""
    _, ext = os.path.splitext(filepath)
    patterns = PATTERNS.get(ext.lower(), [])
    if not patterns:
        return set()

    try:
        with open(filepath, 'r', errors='replace') as f:
            content = f.read()
    except (IOError, OSError):
        return set()

    symbols = set()
    for pattern in patterns:
        for match in pattern.finditer(content):
            name = match.group(1)
            if (name not in NOISE_SYMBOLS
                    and len(name) >= MIN_SYMBOL_LENGTH
                    and not name.startswith('_')):
                symbols.add(name)
    return symbols


def find_referencing_files(workspace: str, symbols: set[str],
                           changed_files: set[str]) -> set[str]:
    """Find files that reference any of the given symbols via grep."""
    if not symbols:
        return set()

    related = set()

    # Build a grep pattern that matches any of the symbols as whole words
    # Use grep -rl for speed (just list matching files, don't show lines)
    # Process in batches to avoid ARG_MAX limits
    symbol_list = sorted(symbols)
    batch_size = 50

    for i in range(0, len(symbol_list), batch_size):
        batch = symbol_list[i:i + batch_size]
        # Build alternation pattern: \bSymbol1\b|\bSymbol2\b|...
        pattern = '|'.join(rf'\b{re.escape(s)}\b' for s in batch)

        try:
            result = subprocess.run(
                ['grep', '-rlE', '--include=*.php', '--include=*.rs',
                 '--include=*.py', '--include=*.js', '--include=*.ts',
                 '--include=*.jsx', '--include=*.tsx',
                 '--exclude-dir=vendor', '--exclude-dir=node_modules',
                 '--exclude-dir=target', '--exclude-dir=.git',
                 '--exclude-dir=build', '--exclude-dir=dist',
                 pattern, workspace],
                capture_output=True, text=True, timeout=30
            )
        except (subprocess.TimeoutExpired, OSError):
            continue

        for line in result.stdout.strip().split('\n'):
            line = line.strip()
            if not line:
                continue
            # Convert absolute path to repo-relative
            if line.startswith(workspace):
                rel = line[len(workspace):].lstrip('/')
            else:
                rel = line
            # Exclude changed files themselves — we only want dependents
            if rel not in changed_files:
                related.add(rel)

    return related


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <workspace_dir> <changed_files_path>",
              file=sys.stderr)
        sys.exit(1)

    workspace = sys.argv[1].rstrip('/')
    changed_files_path = sys.argv[2]

    with open(changed_files_path, 'r') as f:
        changed_files = set(line.strip() for line in f if line.strip())

    if not changed_files:
        sys.exit(0)

    # Extract symbols from all changed files
    all_symbols: set[str] = set()
    for cf in changed_files:
        full_path = os.path.join(workspace, cf)
        symbols = extract_symbols(full_path)
        if symbols:
            print(f"  {cf}: {len(symbols)} symbols ({', '.join(sorted(symbols)[:5])}{'...' if len(symbols) > 5 else ''})",
                  file=sys.stderr)
        all_symbols.update(symbols)

    if not all_symbols:
        print(f"No symbols extracted from {len(changed_files)} changed file(s)",
              file=sys.stderr)
        sys.exit(0)

    print(f"Searching for {len(all_symbols)} symbol(s) across workspace...",
          file=sys.stderr)

    # Find files referencing those symbols
    related = find_referencing_files(workspace, all_symbols, changed_files)

    print(f"Found {len(related)} related file(s)", file=sys.stderr)

    # Output related files, one per line
    for f in sorted(related):
        print(f)


if __name__ == '__main__':
    main()
