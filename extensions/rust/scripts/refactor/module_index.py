"""Module index generation for Rust.

Generates `mod.rs` content when decompose splits a file into a directory of modules.
"""

from typing import List, Union


def generate_module_index(
    submodules: Union[List[str], List[dict]], 
    remaining_content: str = ""
) -> dict:
    """Generate a Rust mod.rs file that wires up submodules and re-exports their public API.

    Args:
        submodules: Either a list of module names (strings) or list of dicts with 
                    'name' and 'pub_items' keys. When just names, uses glob re-exports.
        remaining_content: Any remaining content from the original file (impl blocks, etc.)

    Returns:
        dict with 'content' key containing the mod.rs file content
    """
    lines = []

    # Normalize input to list of dicts
    subs = []
    for sub in submodules:
        if isinstance(sub, str):
            subs.append({"name": sub, "pub_items": None})
        else:
            subs.append(sub)

    # Module declarations
    for sub in subs:
        name = sub.get("name", "") if isinstance(sub, dict) else str(sub)
        if not name:
            continue
        lines.append(f"mod {name};")

    if lines:
        lines.append("")  # Blank line after mod declarations

    # Re-exports
    for sub in subs:
        name = sub.get("name", "") if isinstance(sub, dict) else str(sub)
        if not name:
            continue

        pub_items = sub.get("pub_items") if isinstance(sub, dict) else None

        if pub_items and isinstance(pub_items, list) and len(pub_items) > 0:
            # Explicit re-exports
            if len(pub_items) == 1:
                lines.append(f"pub use {name}::{pub_items[0]};")
            elif len(pub_items) <= 3:
                items_str = ", ".join(pub_items)
                lines.append(f"pub use {name}::{{{items_str}}};")
            else:
                lines.append(f"pub use {name}::{{")
                for item in sorted(pub_items):
                    lines.append(f"    {item},")
                lines.append("};")
        else:
            # Glob re-export (safest for unknown visibility)
            lines.append(f"pub use {name}::*;")

    # Add remaining content (impl blocks, private items, etc.)
    # Inner doc comments (//!) must appear before any mod/use declarations
    # in Rust, so we extract them and prepend them.
    if remaining_content and remaining_content.strip():
        remaining_lines = remaining_content.rstrip().split('\n')
        doc_comments = []
        other_lines = []
        in_doc_prefix = True
        for rline in remaining_lines:
            stripped = rline.strip()
            if in_doc_prefix and stripped.startswith("//!"):
                doc_comments.append(rline)
            else:
                in_doc_prefix = False
                other_lines.append(rline)

        if doc_comments:
            # Doc comments go at the very top, before mod declarations
            doc_block = doc_comments + [""]
            lines = doc_block + lines

        if other_lines and '\n'.join(other_lines).strip():
            if lines:
                lines.append("")  # Blank line separator
            lines.append('\n'.join(other_lines).rstrip())

    # Ensure trailing newline
    content = "\n".join(lines)
    if content and not content.endswith("\n"):
        content += "\n"

    return {"content": content}
