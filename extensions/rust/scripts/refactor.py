#!/usr/bin/env python3
"""Rust refactor script — language-specific parsing for homeboy refactor move.

Receives JSON commands on stdin, outputs JSON results on stdout.

Commands:
  parse_items        — Parse all top-level items in a Rust source file
  resolve_imports    — Determine what imports the destination needs
  find_related_tests — Find test functions related to named items
  adjust_visibility  — Adjust visibility for cross-module moves
  rewrite_caller_imports — Rewrite import paths in caller files
"""

import json
import re
import sys
from dataclasses import dataclass, asdict
from typing import Optional


# ============================================================================
# Data Types
# ============================================================================

@dataclass
class ParsedItem:
    name: str
    kind: str
    start_line: int  # 1-indexed
    end_line: int    # 1-indexed, inclusive
    source: str
    visibility: str = ""


# ============================================================================
# Rust Source Parsing
# ============================================================================

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


# ============================================================================
# Test Detection (#336)
# ============================================================================

def find_test_module(content: str) -> Optional[tuple[int, int, str]]:
    """Find the #[cfg(test)] mod tests block, return (start, end, source)."""
    lines = content.split('\n')

    # Look for #[cfg(test)] followed by mod tests
    for i in range(len(lines)):
        if '#[cfg(test)]' in lines[i]:
            # Look ahead for mod tests or mod <name>
            for j in range(i + 1, min(i + 3, len(lines))):
                if re.match(r'\s*mod\s+\w+\s*\{', lines[j]):
                    end = find_matching_brace(lines, j)
                    return (i, end, '\n'.join(lines[i:end + 1]))

    # Also check for `mod tests {` without #[cfg(test)]
    for i in range(len(lines)):
        if re.match(r'\s*mod\s+tests\s*\{', lines[i].strip()):
            end = find_matching_brace(lines, i)
            return (i, end, '\n'.join(lines[i:end + 1]))

    return None


def parse_test_functions(test_block: str) -> list[ParsedItem]:
    """Parse individual test functions from a test module block."""
    # Strip the outer `mod tests { ... }` wrapper
    lines = test_block.split('\n')
    # Find the opening brace
    inner_start = 0
    for i, line in enumerate(lines):
        if '{' in line:
            inner_start = i + 1
            break
    # Remove last line (closing brace) and outer structure
    inner_lines = lines[inner_start:-1] if len(lines) > inner_start else []
    inner_content = '\n'.join(inner_lines)

    tests = []
    inner_line_list = inner_content.split('\n')
    i = 0

    while i < len(inner_line_list):
        line = inner_line_list[i].strip()

        # Look for #[test] attribute
        if line == '#[test]':
            # Collect preceding doc comments
            doc_start = i
            # Look backward for doc comments (they'd be above #[test])
            while doc_start > 0 and inner_line_list[doc_start - 1].strip().startswith("///"):
                doc_start -= 1

            # Find the fn declaration after #[test]
            fn_line = i + 1
            while fn_line < len(inner_line_list):
                fl = inner_line_list[fn_line].strip()
                if fl.startswith("#[") or not fl:
                    fn_line += 1
                    continue
                break

            if fn_line < len(inner_line_list):
                fn_decl = inner_line_list[fn_line].strip()
                fn_match = re.search(r'\bfn\s+(\w+)', fn_decl)
                if fn_match:
                    name = fn_match.group(1)
                    end = find_matching_brace(inner_line_list, fn_line)

                    # Dedent the source (remove common leading whitespace)
                    raw_lines = inner_line_list[doc_start:end + 1]
                    source = dedent_lines(raw_lines)

                    tests.append(ParsedItem(
                        name=name,
                        kind="test",
                        start_line=doc_start + 1,
                        end_line=end + 1,
                        source=source,
                        visibility="",
                    ))
                    i = end + 1
                    continue
        i += 1

    return tests


def dedent_lines(lines: list[str]) -> str:
    """Remove common leading whitespace from lines."""
    non_empty = [l for l in lines if l.strip()]
    if not non_empty:
        return '\n'.join(lines)

    min_indent = min(len(l) - len(l.lstrip()) for l in non_empty)
    dedented = []
    for l in lines:
        if l.strip():
            dedented.append(l[min_indent:])
        else:
            dedented.append("")
    return '\n'.join(dedented)


def find_related_tests(item_names: list[str], content: str) -> dict:
    """Find test functions that reference the moved items."""
    test_block = find_test_module(content)
    if test_block is None:
        return {"tests": [], "ambiguous": []}

    _, _, block_source = test_block
    all_tests = parse_test_functions(block_source)

    # For each test, check which moved items it references
    related = []
    ambiguous = []

    # Get all top-level item names (not just moved ones) to detect ambiguity
    all_items = parse_items(content)
    all_item_names = {item.name for item in all_items}
    moved_set = set(item_names)
    unmoved_items = all_item_names - moved_set

    for test in all_tests:
        # Check which items the test body references
        refs_moved = [name for name in item_names if re.search(r'\b' + re.escape(name) + r'\b', test.source)]
        refs_unmoved = [name for name in unmoved_items if re.search(r'\b' + re.escape(name) + r'\b', test.source)]

        if refs_moved and not refs_unmoved:
            # Test only references moved items — safe to move
            related.append(test)
        elif refs_moved and refs_unmoved:
            # Test references both moved and unmoved items — ambiguous
            ambiguous.append(test.name)

    # Adjust line numbers to be relative to the full file.
    # parse_test_functions returns line numbers relative to the inner content
    # (after stripping the `#[cfg(test)] mod tests {` wrapper).
    # We need to add the block start offset PLUS the inner content offset.
    block_start, _, block_source = test_block
    block_lines = block_source.split('\n')
    inner_offset = 0
    for bl in block_lines:
        if '{' in bl:
            inner_offset += 1
            break
        inner_offset += 1

    for test in related:
        test.start_line += block_start + inner_offset
        test.end_line += block_start + inner_offset

    return {
        "tests": [asdict(t) for t in related],
        "ambiguous": ambiguous,
    }


# ============================================================================
# Import Resolution (#337, #339, #340, #341)
# ============================================================================

def extract_use_names(use_stmt: str) -> list[str]:
    """Extract terminal name(s) from a Rust use statement."""
    names = []
    body = use_stmt.strip()
    if body.startswith("use "):
        body = body[4:]
    body = body.rstrip(';').strip()

    # Grouped imports
    brace_start = body.find('{')
    if brace_start != -1:
        brace_end = body.find('}')
        if brace_end != -1:
            inner = body[brace_start + 1:brace_end]
            for segment in inner.split(','):
                name = segment.strip()
                if name == "self":
                    continue
                if " as " in name:
                    names.append(name.split(" as ")[1].strip())
                else:
                    names.append(name)
    else:
        # Simple import
        last = body.rsplit("::", 1)[-1].strip()
        if " as " in last:
            names.append(last.split(" as ")[1].strip())
        elif last != "*":
            names.append(last)

    return names


def resolve_imports(moved_items: list[dict], source_content: str, source_path: str, dest_path: str) -> dict:
    """Resolve imports needed in the destination file.

    Handles:
    - Carrying over use statements from source that moved items reference (#340 filtering)
    - Adding imports for same-module types that were in scope in source (#339)
    - Correcting import paths relative to destination (#341)
    """
    source_lines = source_content.split('\n')
    needed = []
    warnings = []

    # Collect all use statements from source
    source_uses = [l.strip() for l in source_lines if l.strip().startswith("use ")]

    # Collect all type definitions in source file (for same-module type refs)
    source_types = set()
    for line in source_lines:
        trimmed = line.strip()
        for prefix in ("pub struct ", "pub(crate) struct ", "struct ",
                        "pub enum ", "pub(crate) enum ", "enum ",
                        "pub type ", "pub(crate) type ", "type ",
                        "pub trait ", "pub(crate) trait ", "trait "):
            if trimmed.startswith(prefix):
                rest = trimmed[len(prefix):]
                name = re.split(r'[{(<;:\s]', rest)[0].strip()
                if name:
                    source_types.add(name)

    # Combined source of all moved items
    combined_source = '\n'.join(item.get("source", "") for item in moved_items)
    moved_item_names = {item.get("name", "") for item in moved_items}

    # Phase 1: Carry needed use statements (with proper filtering for #340)
    for use_stmt in source_uses:
        names = extract_use_names(use_stmt)
        # Check if any terminal name is actually used in the moved items
        # Use word-boundary matching to avoid false positives
        is_needed = any(
            re.search(r'\b' + re.escape(name) + r'\b', combined_source)
            for name in names
            if name not in moved_item_names  # Don't import the item itself
        )
        if is_needed:
            # Fix the import path relative to destination (#341)
            fixed = fix_import_path(use_stmt, source_path, dest_path)
            needed.append(fixed)

    # Phase 2: Add imports for same-module types referenced by moved items (#339)
    source_module = module_stem(source_path)
    dest_module = module_stem(dest_path)
    same_parent = module_parent(source_path) == module_parent(dest_path)

    for type_name in source_types:
        if type_name in moved_item_names:
            continue  # Type is being moved too, no import needed
        # Check if the moved items reference this type
        if re.search(r'\b' + re.escape(type_name) + r'\b', combined_source):
            # Need to add an import for this type
            if same_parent:
                import_line = f"use super::{source_module}::{type_name};"
            else:
                # Different parent — use crate-level path
                source_mod_path = file_to_module_path(source_path)
                import_line = f"use crate::{source_mod_path}::{type_name};"
            # Check it's not already covered by existing use statements
            already_covered = any(type_name in extract_use_names(u) for u in needed)
            if not already_covered:
                needed.append(import_line)

    return {"needed_imports": needed, "warnings": warnings}


def fix_import_path(use_stmt: str, source_path: str, dest_path: str) -> str:
    """Fix a use statement's path relative to the destination file (#341).

    When both source and dest are in the same parent module, `super::` paths
    remain correct. When they differ, we need to adjust.
    """
    source_parent = module_parent(source_path)
    dest_parent = module_parent(dest_path)

    if source_parent == dest_parent:
        # Same parent — super:: paths are still valid
        return use_stmt

    # Different parents — convert super:: paths to crate:: paths
    trimmed = use_stmt.strip().rstrip(';')
    if not trimmed.startswith("use "):
        return use_stmt

    path_part = trimmed[4:].strip()

    if path_part.startswith("super::"):
        # Resolve the super:: relative to the source file
        resolved = resolve_super_path(path_part, source_path)
        if resolved:
            return f"use {resolved};"

    return use_stmt


def resolve_super_path(path: str, source_path: str) -> Optional[str]:
    """Resolve a super:: path relative to a source file into a crate:: path."""
    parts = path.split("::")
    source_modules = file_to_module_path(source_path).split("::")

    # Count how many super:: prefixes
    super_count = 0
    for part in parts:
        if part == "super":
            super_count += 1
        else:
            break

    # Navigate up from source module
    if super_count > len(source_modules):
        return None  # Can't go above crate root

    base = source_modules[:len(source_modules) - super_count]
    remaining = parts[super_count:]

    resolved = base + remaining
    return "crate::" + "::".join(resolved) if resolved else None


def module_stem(file_path: str) -> str:
    """Get the module name from a file path (e.g., 'conventions' from 'src/core/conventions.rs')."""
    import os
    base = os.path.basename(file_path)
    name = os.path.splitext(base)[0]
    if name == "mod":
        return os.path.basename(os.path.dirname(file_path))
    return name


def module_parent(file_path: str) -> str:
    """Get the parent module path (e.g., 'src/core/code_audit' from 'src/core/code_audit/conventions.rs')."""
    import os
    base = os.path.basename(file_path)
    name = os.path.splitext(base)[0]
    parent = os.path.dirname(file_path)
    if name == "mod":
        return os.path.dirname(parent)
    return parent


def file_to_module_path(file_path: str) -> str:
    """Convert file path to Rust module path (e.g., 'src/core/audit/conv.rs' -> 'core::audit::conv')."""
    import os
    p = file_path
    if p.startswith("src/") or p.startswith("src\\"):
        p = p[4:]
    name, ext = os.path.splitext(p)
    if name.endswith("/mod") or name.endswith("\\mod"):
        name = name[:-4]
    return name.replace("/", "::").replace("\\", "::")


# ============================================================================
# Visibility Adjustment (#338)
# ============================================================================

def adjust_visibility(items: list[dict], source_path: str, dest_path: str) -> dict:
    """Adjust visibility of items for cross-module use."""
    adjusted = []

    for item in items:
        source = item.get("source", "")
        vis = item.get("visibility", "")
        kind = item.get("kind", "")

        # If item is private (no visibility) and moving to a different module,
        # change to pub(crate) so it remains accessible
        if vis == "" and kind != "impl":
            # Find the declaration line and add pub(crate)
            new_source = add_pub_crate(source, kind)
            adjusted.append({
                "source": new_source,
                "changed": True,
                "original_visibility": "",
                "new_visibility": "pub(crate)",
            })
        else:
            adjusted.append({
                "source": source,
                "changed": False,
                "original_visibility": vis,
                "new_visibility": vis,
            })

    return {"items": adjusted}


def add_pub_crate(source: str, kind: str) -> str:
    """Add pub(crate) to an item's declaration."""
    lines = source.split('\n')

    # Keywords that start a declaration
    keywords = {
        "function": [r'\bfn\b', r'\basync\s+fn\b', r'\bunsafe\s+fn\b', r'\bconst\s+fn\b'],
        "struct": [r'\bstruct\b'],
        "enum": [r'\benum\b'],
        "const": [r'\bconst\b'],
        "static": [r'\bstatic\b'],
        "type_alias": [r'\btype\b'],
        "trait": [r'\btrait\b'],
    }

    patterns = keywords.get(kind, [])
    if not patterns:
        return source

    for i, line in enumerate(lines):
        trimmed = line.lstrip()
        # Skip doc comments and attributes — keywords inside these are not declarations
        if trimmed.startswith("///") or trimmed.startswith("//!") or trimmed.startswith("#["):
            continue
        for pat in patterns:
            m = re.search(pat, trimmed)
            if m:
                # Insert pub(crate) before the keyword
                indent = len(line) - len(trimmed)
                prefix = line[:indent]
                rest = trimmed
                lines[i] = f"{prefix}pub(crate) {rest}"
                return '\n'.join(lines)

    return source


# ============================================================================
# Caller Import Rewriting (#337)
# ============================================================================

def rewrite_caller_imports(item_names: list[str], source_module_path: str,
                            dest_module_path: str, file_content: str,
                            file_path: str) -> dict:
    """Rewrite import paths in a caller file after items have been moved.

    Handles:
    - `use crate::old_module::item` -> `use crate::new_module::item`
    - `use super::old_module::item` -> resolves correctly
    - Grouped imports: splits out moved items into separate import
    """
    lines = file_content.split('\n')
    rewrites = []

    # Build the module path components
    source_parts = source_module_path.split("::")
    dest_parts = dest_module_path.split("::")
    source_module_name = source_parts[-1] if source_parts else ""
    dest_module_name = dest_parts[-1] if dest_parts else ""

    for i, line in enumerate(lines):
        trimmed = line.strip()
        if not trimmed.startswith("use "):
            continue

        # Check if this use statement references the source module and any moved items
        names = extract_use_names(trimmed)

        # Does this import reference any moved items from the source module?
        has_moved_refs = any(name in item_names for name in names)
        if not has_moved_refs:
            continue

        # Check if the path points to the source module
        path_body = trimmed[4:].rstrip(';').strip()

        # Match various path patterns
        references_source = False

        # crate::path::to::source_module::Item or crate::path::to::source_module::{Item, ...}
        crate_prefix = "crate::" + source_module_path
        if path_body.startswith(crate_prefix):
            references_source = True

        # super::source_module::Item — resolve relative to caller's position
        if "super::" in path_body and source_module_name in path_body:
            # Check if resolving the super path leads to the source module
            resolved = resolve_super_import(path_body, file_path)
            if resolved and resolved.startswith("crate::" + source_module_path):
                references_source = True

        # Direct module reference without crate:: (e.g., `use source_module::Item` in same parent)
        if path_body.startswith(source_module_name + "::"):
            references_source = True

        if not references_source:
            continue

        # Now rewrite the import
        new_line = rewrite_single_import(
            line, item_names, source_module_path, dest_module_path
        )

        if new_line and new_line != line:
            rewrites.append({
                "line": i + 1,  # 1-indexed
                "original": line,
                "replacement": new_line,
            })

    return {"rewrites": rewrites}


def resolve_super_import(path: str, file_path: str) -> Optional[str]:
    """Resolve a super:: import path to a crate:: path."""
    return resolve_super_path(path, file_path)


def rewrite_single_import(line: str, item_names: list[str],
                           source_module_path: str, dest_module_path: str) -> Optional[str]:
    """Rewrite a single use statement, replacing source module with dest module for moved items."""
    trimmed = line.strip()
    indent = len(line) - len(line.lstrip())
    prefix = line[:indent]

    body = trimmed[4:].rstrip(';').strip()

    # Handle grouped imports: `use crate::source_mod::{ItemA, ItemB, ItemC};`
    brace_start = body.find('{')
    if brace_start != -1:
        brace_end = body.find('}')
        if brace_end != -1:
            path_prefix = body[:brace_start].rstrip(':').rstrip()
            inner = body[brace_start + 1:brace_end]
            items_in_group = [s.strip() for s in inner.split(',') if s.strip()]

            moved_in_group = [name for name in items_in_group if name in item_names]
            staying_in_group = [name for name in items_in_group if name not in item_names]

            if not moved_in_group:
                return None  # Nothing to rewrite

            result_lines = []

            # Keep the remaining items in the original import
            if staying_in_group:
                if len(staying_in_group) == 1:
                    result_lines.append(f"{prefix}use {path_prefix}::{staying_in_group[0]};")
                else:
                    result_lines.append(f"{prefix}use {path_prefix}::{{{', '.join(staying_in_group)}}};")

            # Add new import for moved items
            new_path = path_prefix.replace(source_module_path, dest_module_path)
            # Also handle the case where only the last segment differs
            if new_path == path_prefix:
                source_parts = source_module_path.split("::")
                dest_parts_list = dest_module_path.split("::")
                if source_parts[-1] in path_prefix:
                    new_path = path_prefix.replace(source_parts[-1], dest_parts_list[-1])

            if len(moved_in_group) == 1:
                result_lines.append(f"{prefix}use {new_path}::{moved_in_group[0]};")
            else:
                result_lines.append(f"{prefix}use {new_path}::{{{', '.join(moved_in_group)}}};")

            return '\n'.join(result_lines)

    # Simple import: `use crate::source_mod::Item;`
    # Replace the source module path segment with dest module path
    new_body = body.replace(source_module_path, dest_module_path)
    if new_body == body:
        # Try replacing just the last segment
        source_parts = source_module_path.split("::")
        dest_parts_list = dest_module_path.split("::")
        if source_parts[-1] in body:
            new_body = body.replace(source_parts[-1], dest_parts_list[-1])

    if new_body != body:
        return f"{prefix}use {new_body};"
    return None


# ============================================================================
# Command Dispatch
# ============================================================================

def handle_parse_items(data: dict) -> dict:
    content = data.get("content", "")
    items = parse_items(content)
    return {"items": [asdict(item) for item in items]}


def handle_resolve_imports(data: dict) -> dict:
    moved_items = data.get("moved_items", [])
    source_content = data.get("source_content", "")
    source_path = data.get("source_path", "")
    dest_path = data.get("dest_path", "")
    return resolve_imports(moved_items, source_content, source_path, dest_path)


def handle_find_related_tests(data: dict) -> dict:
    item_names = data.get("item_names", [])
    content = data.get("content", "")
    return find_related_tests(item_names, content)


def handle_adjust_visibility(data: dict) -> dict:
    items = data.get("items", [])
    source_path = data.get("source_path", "")
    dest_path = data.get("dest_path", "")
    return adjust_visibility(items, source_path, dest_path)


def handle_rewrite_caller_imports(data: dict) -> dict:
    item_names = data.get("item_names", [])
    source_module_path = data.get("source_module_path", "")
    dest_module_path = data.get("dest_module_path", "")
    file_content = data.get("file_content", "")
    file_path = data.get("file_path", "")
    return rewrite_caller_imports(item_names, source_module_path, dest_module_path,
                                   file_content, file_path)


# ============================================================================
# Struct Field Propagation
# ============================================================================

def parse_struct_fields(struct_source: str) -> list[dict]:
    """Parse a Rust struct definition and extract field names, types, and defaults.

    Handles:
      - pub field: Type,
      - pub(crate) field: Type,
      - field: Type,
      - /// doc comments (skipped)
      - #[serde(default)] attributes → marks field as having a default
    """
    fields = []
    lines = struct_source.split('\n')
    has_default_attr = False

    for line in lines:
        stripped = line.strip()

        # Track #[serde(default)] or #[default] on the next field
        if stripped.startswith('#[') and 'default' in stripped.lower():
            has_default_attr = True
            continue

        # Skip doc comments, empty lines, braces
        if stripped.startswith('//') or stripped == '' or stripped in ('{', '}'):
            has_default_attr = False
            continue

        # Match field: pub name: Type, or name: Type,
        field_match = re.match(
            r'(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*(.+?)\s*,?\s*$',
            stripped
        )
        if field_match:
            field_name = field_match.group(1)
            field_type = field_match.group(2).rstrip(',').strip()

            # Infer a sensible default from the type
            default_value = infer_default(field_type)

            fields.append({
                'name': field_name,
                'type': field_type,
                'default': default_value,
                'has_serde_default': has_default_attr,
            })
            has_default_attr = False

    return fields


def infer_default(rust_type: str) -> str:
    """Infer a sensible default value for a Rust type."""
    t = rust_type.strip()

    # Option<T> → None
    if t.startswith('Option<'):
        return 'None'

    # Vec<T> → vec![]
    if t.startswith('Vec<') or t == 'Vec':
        return 'vec![]'

    # HashMap/BTreeMap → T::new()
    if 'HashMap' in t or 'BTreeMap' in t:
        return 'std::collections::HashMap::new()' if 'HashMap' in t else 'std::collections::BTreeMap::new()'

    # HashSet/BTreeSet
    if 'HashSet' in t or 'BTreeSet' in t:
        return 'std::collections::HashSet::new()' if 'HashSet' in t else 'std::collections::BTreeSet::new()'

    # String → String::new()
    if t == 'String':
        return 'String::new()'

    # bool → false
    if t == 'bool':
        return 'false'

    # Numeric types → 0
    if t in ('u8', 'u16', 'u32', 'u64', 'u128', 'usize',
             'i8', 'i16', 'i32', 'i64', 'i128', 'isize',
             'f32', 'f64'):
        return '0'

    # Fallback: Default::default()
    return 'Default::default()'


def find_struct_instantiations(content: str, struct_name: str) -> list[dict]:
    """Find all instantiations of a struct in file content.

    Returns list of {start_line, end_line, fields_present, indent} for each.
    """
    instantiations = []
    lines = content.split('\n')

    for i, line in enumerate(lines):
        # Match: StructName { or StructName{
        pattern = re.escape(struct_name) + r'\s*\{'
        match = re.search(pattern, line)
        if not match:
            continue

        # Check this isn't a struct definition or function return type
        before = line[:match.start()].strip()
        if before.endswith('struct') or before.endswith('enum') or 'struct ' in before:
            continue
        # Skip function signatures: -> StructName {
        if '->' in before:
            continue
        # Skip type aliases, trait definitions
        if before.startswith('type ') or before.startswith('trait '):
            continue

        # Find the closing brace
        depth = 0
        found_open = False
        end_line = i
        for j in range(i, len(lines)):
            for ch in lines[j]:
                if ch == '{':
                    depth += 1
                    found_open = True
                elif ch == '}':
                    depth -= 1
            if found_open and depth == 0:
                end_line = j
                break

        # Extract field names present in this instantiation
        block = '\n'.join(lines[i:end_line + 1])
        fields_present = set()
        for fm in re.finditer(r'(\w+)\s*:', block):
            field_name = fm.group(1)
            # Skip the struct name itself and common keywords
            if field_name != struct_name and field_name not in ('pub', 'crate', 'self', 'super'):
                fields_present.add(field_name)

        # Detect indentation of fields inside the struct
        field_indent = '            '  # default 12 spaces
        for j in range(i + 1, end_line + 1):
            stripped = lines[j].strip()
            if stripped and not stripped.startswith('//') and ':' in stripped:
                leading = len(lines[j]) - len(lines[j].lstrip())
                field_indent = lines[j][:leading]
                break

        instantiations.append({
            'start_line': i + 1,  # 1-indexed
            'end_line': end_line + 1,
            'fields_present': fields_present,
            'indent': field_indent,
            'closing_brace_line': end_line + 1,
        })

    return instantiations


def propagate_struct_fields(data: dict) -> dict:
    """Given a struct's fields and a file, return edits to add missing fields.

    Input:
      struct_name: str — name of the struct
      struct_fields: list[{name, type, default}] — full field list (from definition)
      file_content: str — content of file to fix
      file_path: str — path for reporting

    Output:
      edits: list[{line, column, insert_text, description}]
    """
    struct_name = data['struct_name']
    struct_fields = data.get('struct_fields', [])
    file_content = data['file_content']
    file_path = data.get('file_path', '')

    # If struct_fields not provided, try to parse from struct_source
    if not struct_fields and 'struct_source' in data:
        struct_fields = parse_struct_fields(data['struct_source'])

    instantiations = find_struct_instantiations(file_content, struct_name)
    lines = file_content.split('\n')

    edits = []
    for inst in instantiations:
        missing = []
        for field in struct_fields:
            if field['name'] not in inst['fields_present']:
                missing.append(field)

        if not missing:
            continue

        # Insert missing fields before the closing brace
        insert_line = inst['closing_brace_line']
        indent = inst['indent']

        for field in missing:
            default = field.get('default', 'Default::default()')
            insert_text = f"{indent}{field['name']}: {default},"
            edits.append({
                'file': file_path,
                'line': insert_line,
                'insert_text': insert_text,
                'description': f"Add missing field `{field['name']}` to {struct_name} instantiation",
            })

    return {
        'edits': edits,
        'instantiations_found': len(instantiations),
        'instantiations_needing_fix': len([i for i in instantiations
                                           if any(f['name'] not in i['fields_present']
                                                  for f in struct_fields)]),
    }


def handle_propagate_struct_fields(data: dict) -> dict:
    return propagate_struct_fields(data)


COMMANDS = {
    "parse_items": handle_parse_items,
    "resolve_imports": handle_resolve_imports,
    "find_related_tests": handle_find_related_tests,
    "adjust_visibility": handle_adjust_visibility,
    "rewrite_caller_imports": handle_rewrite_caller_imports,
    "propagate_struct_fields": handle_propagate_struct_fields,
}


def main():
    raw = sys.stdin.read()
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        print(json.dumps({"error": f"Invalid JSON: {e}"}), file=sys.stderr)
        sys.exit(1)

    command = data.get("command", "")
    handler = COMMANDS.get(command)
    if handler is None:
        print(json.dumps({"error": f"Unknown command: {command}"}), file=sys.stderr)
        sys.exit(1)

    try:
        result = handler(data)
        print(json.dumps(result))
    except Exception as e:
        print(json.dumps({"error": str(e)}), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
