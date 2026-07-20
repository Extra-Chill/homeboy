"""Rust struct-definition discovery.

Language-specific analysis for locating a struct definition and extracting its
full source block (attributes + doc comments + braced body) from Rust source.

This lives in the Rust refactor extension — not in `homeboy-refactor` — so the
core refactor engine stays language-agnostic. The engine walks candidate files
(by extension) and asks each language extension, via the `find_definition`
command, whether a given file defines the named item and what its source block
is. A PHP/Go/Swift extension answers the same command with its own class/struct
syntax; nothing about `pub struct` leaks into the generic dispatch.
"""


def defines_struct(content: str, struct_name: str) -> bool:
    """Whether `content` contains a definition of `struct_name`.

    Matches `pub struct Name`, `pub(crate) struct Name`, and the immediate
    brace form (`... struct Name {`). Mirrors the historical core behavior.
    """
    patterns = (
        f"pub struct {struct_name} ",
        f"pub struct {struct_name} {{",
        f"pub(crate) struct {struct_name} ",
        f"pub(crate) struct {struct_name} {{",
    )
    return any(p in content for p in patterns)


def extract_struct_source(content: str, struct_name: str):
    """Extract the full struct source block from `content`.

    Walks backwards from the `struct Name` line to include leading attributes
    (`#[...]`) and doc comments (`///`, `//!`), then forwards to the
    brace-balanced end of the struct body. Returns the source string, or `None`
    if the struct is not found.
    """
    lines = content.split("\n")

    struct_pattern = f"struct {struct_name} "
    struct_pattern_brace = f"struct {struct_name} {{"
    start_line = None

    for i, line in enumerate(lines):
        if struct_pattern in line or struct_pattern_brace in line:
            # Walk backwards to include attributes and doc comments.
            actual_start = i
            for j in range(i - 1, -1, -1):
                trimmed = lines[j].strip()
                if (
                    trimmed.startswith("#")
                    or trimmed.startswith("///")
                    or trimmed.startswith("//!")
                ):
                    actual_start = j
                elif trimmed == "":
                    prev = lines[j - 1].strip() if j > 0 else ""
                    if j > 0 and (prev.startswith("#") or prev.startswith("///")):
                        actual_start = j
                    else:
                        break
                else:
                    break
            start_line = actual_start
            break

    if start_line is None:
        return None

    # Find the brace-balanced end of the struct body.
    depth = 0
    found_open = False
    end_line = start_line
    for i in range(start_line, len(lines)):
        for ch in lines[i]:
            if ch == "{":
                depth += 1
                found_open = True
            elif ch == "}":
                depth -= 1
        if found_open and depth == 0:
            end_line = i
            break

    return "\n".join(lines[start_line : end_line + 1])


def find_definition(data: dict) -> dict:
    """Answer whether a single file defines the named struct.

    Input:
        struct_name: str
        file_content: str
        file_path: str (opaque; echoed back for the caller's convenience)

    Output:
        defines: bool
        struct_source: str | None  — the full source block when `defines`
        file_path: str
    """
    struct_name = data["struct_name"]
    content = data.get("file_content", "")
    file_path = data.get("file_path", "")

    if not defines_struct(content, struct_name):
        return {"defines": False, "struct_source": None, "file_path": file_path}

    source = extract_struct_source(content, struct_name)
    # `defines_struct` uses `pub`-qualified patterns; `extract_struct_source`
    # matches the bare `struct Name` line. They agree in practice, but guard
    # against a mismatch by reporting `defines` only when we actually extracted.
    return {
        "defines": source is not None,
        "struct_source": source,
        "file_path": file_path,
    }
