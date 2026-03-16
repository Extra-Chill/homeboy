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

    # Collect all definitions in source file (types, functions, constants).
    # These are items that were in scope for the moved code when it lived
    # in the source file — the destination file needs imports for any it references.
    source_definitions = set()
    for line in source_lines:
        trimmed = line.strip()
        # Strip visibility
        rest = trimmed
        for vis in ("pub(crate) ", "pub(super) ", "pub "):
            if rest.startswith(vis):
                rest = rest[len(vis):]
                break
        # Strip function modifiers (async, unsafe) but NOT const —
        # const can be both a modifier (`const fn`) and a keyword (`const X`).
        for modifier in ("async ", "unsafe "):
            if rest.startswith(modifier):
                rest = rest[len(modifier):]
        for keyword in ("struct ", "enum ", "type ", "trait ", "fn ",
                        "const ", "static "):
            if rest.startswith(keyword):
                after = rest[len(keyword):]
                # For `const fn`, extract the function name after `fn`
                if keyword == "const " and after.startswith("fn "):
                    after = after[3:]
                name = re.split(r'[{(<;:\s]', after)[0].strip()
                if name:
                    source_definitions.add(name)
                break

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

    # Phase 2: Add imports for same-module definitions referenced by moved items (#339)
    # This covers types, functions, and constants that were in scope because
    # the moved code lived in the same file.
    source_mod = module_stem(source_path)
    same_parent = module_parent(source_path) == module_parent(dest_path)

    for def_name in source_definitions:
        if def_name in moved_item_names:
            continue  # Item is being moved too, no import needed
        # Check if the moved items reference this definition
        if re.search(r'\b' + re.escape(def_name) + r'\b', combined_source):
            # Need to add an import for this definition
            if same_parent:
                import_line = f"use super::{source_mod}::{def_name};"
            else:
                # Different parent — use crate-level path
                source_mod_path = file_to_module_path(source_path)
                import_line = f"use crate::{source_mod_path}::{def_name};"
            # Check it's not already covered by existing use statements
            already_covered = any(def_name in extract_use_names(u) for u in needed)
            if not already_covered:
                needed.append(import_line)

    # Phase 3: Carry forward glob imports when moved code has unresolved references.
    # When the source has `use foo::*;`, we can't statically resolve what symbols
    # it provides. But if the moved code references identifiers that aren't covered
    # by Phase 1 (explicit use statements) or Phase 2 (same-module definitions),
    # the glob import is likely providing them.
    glob_uses = [u for u in source_uses if u.rstrip(';').strip().endswith("::*")]
    if glob_uses:
        # Collect all names already covered by Phase 1 + Phase 2
        covered_names = set()
        for use_stmt in needed:
            covered_names.update(extract_use_names(use_stmt))
        covered_names.update(moved_item_names)
        covered_names.update(source_definitions)

        # Find identifiers in the moved code that look like they could be
        # unresolved references (uppercase constants or PascalCase types
        # are the most common glob-provided symbols)
        all_idents = set(re.findall(r'\b([A-Z][A-Z_0-9]+|[A-Z][a-zA-Z0-9]+)\b', combined_source))
        # Remove Rust keywords and known types
        rust_builtins = {"Some", "None", "Ok", "Err", "Self", "Vec", "String",
                         "Option", "Result", "Box", "Arc", "Rc", "HashMap",
                         "HashSet", "Path", "PathBuf", "BTreeMap", "BTreeSet",
                         "Cow", "Pin", "Future", "Iterator", "Display", "Debug",
                         "Default", "Clone", "Copy", "Send", "Sync", "Sized",
                         "From", "Into", "AsRef", "AsMut", "Deref", "Drop",
                         "Fn", "FnMut", "FnOnce", "ToOwned", "ToString",
                         "TRUE", "FALSE", "NULL"}
        unresolved = all_idents - covered_names - rust_builtins

        if unresolved:
            for glob_use in glob_uses:
                fixed = fix_import_path(glob_use, source_path, dest_path)
                if fixed not in needed:
                    needed.append(fixed)

    return {"needed_imports": needed, "warnings": warnings}
