"""Import resolution — determine what imports moved items need.

Handles:
- Carrying over use statements from source that moved items reference
- Adding imports for same-module types that were in scope in source
- Correcting import paths relative to destination
"""

import os
import re
from typing import Optional


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


def module_stem(file_path: str) -> str:
    """Get the module name from a file path (e.g., 'conventions' from 'src/core/conventions.rs')."""
    base = os.path.basename(file_path)
    name = os.path.splitext(base)[0]
    if name == "mod":
        return os.path.basename(os.path.dirname(file_path))
    return name


def module_parent(file_path: str) -> str:
    """Get the parent module path (e.g., 'src/core/code_audit' from 'src/core/code_audit/conventions.rs')."""
    base = os.path.basename(file_path)
    name = os.path.splitext(base)[0]
    parent = os.path.dirname(file_path)
    if name == "mod":
        return os.path.dirname(parent)
    return parent


def file_to_module_path(file_path: str) -> str:
    """Convert file path to Rust module path (e.g., 'src/core/audit/conv.rs' -> 'core::audit::conv')."""
    p = file_path
    if p.startswith("src/") or p.startswith("src\\"):
        p = p[4:]
    name, ext = os.path.splitext(p)
    if name.endswith("/mod") or name.endswith("\\mod"):
        name = name[:-4]
    return name.replace("/", "::").replace("\\", "::")


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


def fix_import_path(use_stmt: str, source_path: str, dest_path: str) -> str:
    """Fix a use statement's path relative to the destination file.

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
    source_mod = module_stem(source_path)
    same_parent = module_parent(source_path) == module_parent(dest_path)

    for type_name in source_types:
        if type_name in moved_item_names:
            continue  # Type is being moved too, no import needed
        # Check if the moved items reference this type
        if re.search(r'\b' + re.escape(type_name) + r'\b', combined_source):
            # Need to add an import for this type
            if same_parent:
                import_line = f"use super::{source_mod}::{type_name};"
            else:
                # Different parent — use crate-level path
                source_mod_path = file_to_module_path(source_path)
                import_line = f"use crate::{source_mod_path}::{type_name};"
            # Check it's not already covered by existing use statements
            already_covered = any(type_name in extract_use_names(u) for u in needed)
            if not already_covered:
                needed.append(import_line)

    return {"needed_imports": needed, "warnings": warnings}
