"""Caller import rewriting — update import paths after items move between modules.

Handles:
- `use crate::old_module::item` -> `use crate::new_module::item`
- `use super::old_module::item` -> resolves correctly
- Grouped imports: splits out moved items into separate import
"""

from typing import Optional

from .imports import extract_use_names, resolve_super_path


def rewrite_caller_imports(item_names: list[str], source_module_path: str,
                            dest_module_path: str, file_content: str,
                            file_path: str) -> dict:
    """Rewrite import paths in a caller file after items have been moved."""
    lines = file_content.split('\n')
    rewrites = []

    # Build the module path components
    source_parts = source_module_path.split("::")
    source_module_name = source_parts[-1] if source_parts else ""

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
            resolved = resolve_super_path(path_body, file_path)
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
