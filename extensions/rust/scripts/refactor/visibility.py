"""Visibility adjustment — add pub(crate) to items crossing module boundaries."""

import re


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
