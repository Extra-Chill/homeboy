"""Test detection — find and parse #[cfg(test)] mod tests blocks.

Handles finding the test module, parsing individual test functions,
and determining which tests are related to moved items.
"""

import re
from dataclasses import asdict
from typing import Optional

from .types import ParsedItem
from .parsing import find_matching_brace, parse_items


def find_test_module(content: str) -> Optional[tuple[int, int, str]]:
    """Find the #[cfg(test)] mod tests block, return (start, end, source)."""
    lines = content.split('\n')

    # Look for #[cfg(test)] followed specifically by mod tests / mod test.
    # Do not treat arbitrary cfg(test) modules as the canonical test module,
    # or we risk hiding unrelated test-only helper items from top-level parsing.
    for i in range(len(lines)):
        if '#[cfg(test)]' in lines[i]:
            # Look ahead for the conventional test module only.
            for j in range(i + 1, min(i + 3, len(lines))):
                if re.match(r'\s*mod\s+tests?\s*\{', lines[j]):
                    end = find_matching_brace(lines, j)
                    return (i, end, '\n'.join(lines[i:end + 1]))

    # Also check for `mod tests {` without #[cfg(test)]
    for i in range(len(lines)):
        if re.match(r'\s*mod\s+tests\s*\{', lines[i].strip()):
            end = find_matching_brace(lines, i)
            return (i, end, '\n'.join(lines[i:end + 1]))

    return None


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
