"""Visibility adjustment — adjust item visibility when crossing module boundaries.

Handles two cases:
- Private items → pub(crate) when moving to a different module
- pub(crate) items → pub when decomposing into submodules (so pub use submod::* works)
"""

import os
import re


def _dest_is_child_of_source(source_path: str, dest_path: str) -> bool:
    """Check if dest is a child module of source (decompose pattern).

    When decomposing `src/core/big.rs` into `src/core/big/helpers.rs`,
    the destination is inside the source module's directory.

    Examples:
        _dest_is_child_of_source("src/core/big.rs", "src/core/big/helpers.rs") -> True
        _dest_is_child_of_source("src/core/big.rs", "src/other/helpers.rs") -> False
    """
    source_stem = os.path.splitext(source_path)[0]  # "src/core/big"
    dest_dir = os.path.dirname(dest_path)  # "src/core/big"
    return os.path.normpath(source_stem) == os.path.normpath(dest_dir)


def adjust_visibility(items: list[dict], source_path: str, dest_path: str) -> dict:
    """Adjust visibility of items for cross-module use."""
    adjusted = []
    is_decompose = _dest_is_child_of_source(source_path, dest_path)

    for item in items:
        source = item.get("source", "")
        vis = item.get("visibility", "")
        kind = item.get("kind", "")

        if kind == "impl":
            # impl blocks don't have their own visibility
            adjusted.append({
                "source": source,
                "changed": False,
                "original_visibility": vis,
                "new_visibility": vis,
            })
        elif vis == "":
            # Private items → pub(crate) so they remain accessible
            new_source = add_pub_crate(source, kind)
            adjusted.append({
                "source": new_source,
                "changed": True,
                "original_visibility": "",
                "new_visibility": "pub(crate)",
            })
        elif vis == "pub(crate)" and is_decompose:
            # Decompose case: pub(crate) → pub so `pub use submod::*` can
            # re-export them. Without this, the glob re-export is invisible
            # because pub(crate) items can't be re-exported through pub use.
            new_source = upgrade_pub_crate_to_pub(source, kind)
            adjusted.append({
                "source": new_source,
                "changed": True,
                "original_visibility": "pub(crate)",
                "new_visibility": "pub",
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


def upgrade_pub_crate_to_pub(source: str, kind: str) -> str:
    """Upgrade pub(crate) to pub on an item's declaration.

    When items move to submodules during decompose, pub(crate) is too
    restrictive for `pub use submod::*` to re-export them. They need
    full `pub` visibility within the submodule so the parent mod.rs
    can re-export them.
    """
    lines = source.split('\n')

    for i, line in enumerate(lines):
        trimmed = line.lstrip()
        # Skip doc comments and attributes
        if trimmed.startswith("///") or trimmed.startswith("//!") or trimmed.startswith("#["):
            continue
        # Match pub(crate) at the start of the declaration line
        if trimmed.startswith("pub(crate) "):
            indent = len(line) - len(trimmed)
            prefix = line[:indent]
            rest = trimmed[len("pub(crate) "):]
            lines[i] = f"{prefix}pub {rest}"
            return '\n'.join(lines)

    return source
