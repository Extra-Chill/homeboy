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
    """Extract terminal name(s) from a Rust use statement.

    For grouped imports with `self` (e.g., `use crate::foo::{self, Bar}`),
    `self` brings the parent path's last segment into scope as a name.
    So `use crate::engine::local_files::{self, FileSystem}` makes
    `local_files` available as a name — we include it.
    """
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
            path_prefix = body[:brace_start].rstrip(':').rstrip()
            inner = body[brace_start + 1:brace_end]
            has_self = False
            for segment in inner.split(','):
                name = segment.strip()
                if name == "self":
                    has_self = True
                    continue
                if " as " in name:
                    names.append(name.split(" as ")[1].strip())
                else:
                    names.append(name)
            # `self` brings the last path segment into scope as a name
            if has_self and "::" in path_prefix:
                module_name = path_prefix.rsplit("::", 1)[-1]
                if module_name:
                    names.append(module_name)
    else:
        # Simple import
        last = body.rsplit("::", 1)[-1].strip()
        if " as " in last:
            names.append(last.split(" as ")[1].strip())
        elif last != "*":
            names.append(last)

    return names


def _dest_is_child_of_source(source_path: str, dest_path: str) -> bool:
    """Check if dest is a child module of source (decompose pattern).

    When decomposing `src/core/big.rs` into `src/core/big/helpers.rs`,
    the destination is inside the source module's directory. In Rust terms,
    `super::` from the dest file points to the source module.

    Examples:
        _dest_is_child_of_source("src/core/big.rs", "src/core/big/helpers.rs") -> True
        _dest_is_child_of_source("src/core/big.rs", "src/other/helpers.rs") -> False
    """
    source_stem = os.path.splitext(source_path)[0]  # "src/core/big"
    dest_dir = os.path.dirname(dest_path)  # "src/core/big"
    # Normalize: source_stem should match dest_dir for the decompose case
    return os.path.normpath(source_stem) == os.path.normpath(dest_dir)


def _is_self_group_import(use_stmt: str) -> bool:
    """Check if a use statement is a grouped import containing `self`."""
    body = use_stmt.strip().rstrip(';')
    if not body.startswith("use "):
        return False
    path = body[4:].strip()
    brace_start = path.find('{')
    if brace_start == -1:
        return False
    brace_end = path.find('}')
    if brace_end == -1:
        return False
    inner = path[brace_start + 1:brace_end]
    items = [s.strip() for s in inner.split(',')]
    return "self" in items


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

    Three cases:
    1. Same parent module — `super::` paths remain correct.
    2. Decompose (dest is child of source) — `super::` from source becomes
       `super::super::` from dest (one extra level).
    3. Different parents — resolve `super::` to absolute `crate::` paths.
    """
    source_parent = module_parent(source_path)
    dest_parent = module_parent(dest_path)

    if source_parent == dest_parent:
        # Same parent — super:: paths are still valid
        return use_stmt

    trimmed = use_stmt.strip().rstrip(';')
    if not trimmed.startswith("use "):
        return use_stmt

    path_part = trimmed[4:].strip()

    if not path_part.startswith("super::"):
        # Not a super:: path — crate:: paths are always valid
        return use_stmt

    # Decompose case: dest is one level deeper than source.
    # `super::X` from source becomes `super::super::X` from dest.
    if _dest_is_child_of_source(source_path, dest_path):
        return f"use super::{path_part};"

    # General case: different parents — resolve to absolute crate:: path
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
        elif _is_self_group_import(use_stmt):
            # For `use foo::{self, Trait}` where `self` brings a module into scope:
            # if the module name (from `self`) is used via path syntax (e.g., `local_files::local()`),
            # carry the ENTIRE grouped import — the trait companions are needed for method dispatch.
            body = use_stmt.strip()
            if body.startswith("use "):
                path = body[4:].rstrip(';').strip()
                brace_idx = path.find('{')
                if brace_idx != -1:
                    prefix = path[:brace_idx].rstrip(':').rstrip()
                    module_name = prefix.rsplit("::", 1)[-1] if "::" in prefix else prefix
                    # Check if the module is used as a path qualifier (e.g., `module_name::something`)
                    if module_name and re.search(r'\b' + re.escape(module_name) + r'::', combined_source):
                        fixed = fix_import_path(use_stmt, source_path, dest_path)
                        if fixed not in needed:
                            needed.append(fixed)

    # Phase 2: Add imports for same-module definitions referenced by moved items (#339)
    # This covers types, functions, and constants that were in scope because
    # the moved code lived in the same file.
    source_mod = module_stem(source_path)
    source_parent = module_parent(source_path)
    dest_parent = module_parent(dest_path)
    same_parent = source_parent == dest_parent

    # Check if dest is a child of source (decompose case: foo.rs -> foo/bar.rs)
    # In this case, `super::` from dest points to source's module.
    dest_is_child = _dest_is_child_of_source(source_path, dest_path)

    for def_name in source_definitions:
        if def_name in moved_item_names:
            continue  # Item is being moved too, no import needed
        # Check if the moved items reference this definition
        if re.search(r'\b' + re.escape(def_name) + r'\b', combined_source):
            # Need to add an import for this definition
            if dest_is_child:
                # Decompose case: dest is inside source's module directory.
                # `super::` from dest points directly to the source module.
                import_line = f"use super::{def_name};"
            elif same_parent:
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
