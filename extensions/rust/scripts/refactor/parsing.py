"""Rust source parsing — item boundary detection and extraction.

Core parsing logic for detecting top-level items (functions, structs, enums,
traits, impls, consts, statics, type aliases) in Rust source files.
"""

import re
from typing import Optional

from .types import ParsedItem


def find_matching_brace(lines: list[str], start_line: int) -> int:
    """Find the line where braces balance to zero, skipping strings/comments."""
    depth = 0
    found_open = False
    in_block_comment = False

    for i in range(start_line, len(lines)):
        line = lines[i]
        j = 0
        chars = list(line)

        while j < len(chars):
            if in_block_comment:
                if j + 1 < len(chars) and chars[j] == '*' and chars[j + 1] == '/':
                    in_block_comment = False
                    j += 2
                else:
                    j += 1
                continue

            # Block comment start
            if j + 1 < len(chars) and chars[j] == '/' and chars[j + 1] == '*':
                in_block_comment = True
                j += 2
                continue

            # Line comment
            if j + 1 < len(chars) and chars[j] == '/' and chars[j + 1] == '/':
                break

            # String literal
            if chars[j] == '"':
                j += 1
                while j < len(chars):
                    if chars[j] == '\\':
                        j += 2
                    elif chars[j] == '"':
                        j += 1
                        break
                    else:
                        j += 1
                continue

            # Character literal (not lifetime annotations like `'static`)
            if chars[j] == "'":
                if j + 1 < len(chars) and (chars[j + 1].isalnum() or chars[j + 1] == '_'):
                    # Rust lifetime annotation — do not treat as char literal.
                    j += 1
                    continue

                j += 1
                while j < len(chars):
                    if chars[j] == '\\':
                        j += 2
                    elif chars[j] == "'":
                        j += 1
                        break
                    else:
                        j += 1
                continue

            if chars[j] == '{':
                depth += 1
                found_open = True
            elif chars[j] == '}':
                depth -= 1
                if found_open and depth == 0:
                    return i

            j += 1

    return len(lines) - 1


def extract_visibility(decl: str) -> tuple[str, str]:
    """Extract visibility prefix from a declaration line."""
    if decl.startswith("pub(crate) "):
        return "pub(crate)", decl[len("pub(crate) "):]
    elif decl.startswith("pub(super) "):
        return "pub(super)", decl[len("pub(super) "):]
    elif decl.startswith("pub "):
        return "pub", decl[4:]
    return "", decl


def parse_item_declaration(decl: str) -> Optional[tuple[str, str, str]]:
    """Parse an item declaration to extract (kind, name, visibility)."""
    vis, rest = extract_visibility(decl)

    # Function declaration (top-level item start only).
    # We intentionally anchor to the declaration line start so we don't
    # mis-detect `fn` in return-position trait bounds like:
    #   Option<std::path::PathBuf> { ... }
    #   where `\bfn\s+` can appear inside tokens such as `PathBuf`.
    fn_match = re.match(r'^(?:async\s+|unsafe\s+|const\s+|extern\s+)*fn\s+(\w+)', rest)
    if fn_match:
        return "function", fn_match.group(1), vis

    # Struct
    m = re.match(r'struct\s+(\w+)', rest)
    if m:
        return "struct", m.group(1), vis

    # Enum
    m = re.match(r'enum\s+(\w+)', rest)
    if m:
        return "enum", m.group(1), vis

    # Const
    m = re.match(r'const\s+(\w+)', rest)
    if m:
        return "const", m.group(1), vis

    # Static
    m = re.match(r'static\s+(\w+)', rest)
    if m:
        return "static", m.group(1), vis

    # Type alias
    m = re.match(r'type\s+(\w+)', rest)
    if m:
        return "type_alias", m.group(1), vis

    # Trait
    m = re.match(r'trait\s+(\w+)', rest)
    if m:
        return "trait", m.group(1), vis

    # Impl
    if rest.startswith("impl"):
        after = rest[4:].strip()
        name = re.split(r'[{<]', after)[0].strip()
        if name:
            return "impl", name, vis

    return None


def find_item_end(lines: list[str], decl_line: int, kind: str) -> int:
    """Find the end line of an item."""
    if kind in ("const", "static", "type_alias"):
        for i in range(decl_line, len(lines)):
            if ';' in lines[i]:
                return i
        return decl_line

    if kind == "struct":
        combined = " ".join(lines[decl_line:decl_line + 3])
        if ';' in combined and '{' not in combined:
            for i in range(decl_line, len(lines)):
                if ';' in lines[i]:
                    return i
            return decl_line

    return find_matching_brace(lines, decl_line)


def parse_items(content: str) -> list[ParsedItem]:
    """Parse all top-level items in a Rust source file.

    Skips items inside `#[cfg(test)] mod tests { ... }` blocks —
    those are handled separately by `find_related_tests`.
    """
    from .tests import find_test_module

    lines = content.split('\n')
    items = []
    i = 0

    # Find the test module range to exclude
    test_range = None
    test_block = find_test_module(content)
    if test_block:
        test_start, test_end, _ = test_block
        test_range = (test_start, test_end)

    while i < len(lines):
        # Skip lines inside test module
        if test_range and test_range[0] <= i <= test_range[1]:
            i = test_range[1] + 1
            continue

        trimmed = lines[i].strip()

        # Skip blank lines, use statements, mod declarations, module docs
        if (not trimmed or trimmed.startswith("use ") or
                trimmed.startswith("mod ") or trimmed.startswith("//!")):
            i += 1
            continue

        # Check for doc comment / attribute prefix
        if trimmed.startswith("///") or trimmed.startswith("#["):
            prefix_start = i
            j = i
            while j < len(lines):
                t = lines[j].strip()
                if t.startswith("///") or t.startswith("#[") or not t:
                    j += 1
                else:
                    break
            if j >= len(lines):
                i += 1
                continue
            decl_line = j

            # Check if we've entered the test module range
            if test_range and test_range[0] <= decl_line <= test_range[1]:
                i = test_range[1] + 1
                continue
        else:
            prefix_start = i
            decl_line = i

        decl = lines[decl_line].strip()
        parsed = parse_item_declaration(decl)
        if parsed is None:
            i += 1
            continue

        kind, name, visibility = parsed
        end_line = find_item_end(lines, decl_line, kind)

        source = '\n'.join(lines[prefix_start:end_line + 1])
        items.append(ParsedItem(
            name=name,
            kind=kind,
            start_line=prefix_start + 1,  # 1-indexed
            end_line=end_line + 1,          # 1-indexed
            source=source,
            visibility=visibility,
        ))
        i = end_line + 1

    return items
