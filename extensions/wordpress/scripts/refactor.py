#!/usr/bin/env python3
"""PHP refactor script — language-specific parsing for homeboy refactor & audit fix.

Receives JSON commands on stdin, outputs JSON results on stdout.

Commands:
  parse_items      — Parse class methods/functions in a PHP source file
  extract_shared   — Generate a shared trait file + usage instructions for duplicates
"""

import json
import re
import sys


# ============================================================================
# PHP Source Parsing
# ============================================================================

def find_matching_brace(text, start_pos):
    """Find the position of the matching closing brace, handling nesting and strings."""
    depth = 0
    i = start_pos
    in_single_quote = False
    in_double_quote = False
    in_heredoc = False
    heredoc_tag = None

    while i < len(text):
        ch = text[i]

        # Handle heredoc/nowdoc
        if in_heredoc:
            # Check for end tag at start of line
            if ch == '\n' and i + 1 < len(text):
                rest = text[i+1:]
                if heredoc_tag and rest.startswith(heredoc_tag):
                    end_pos = i + 1 + len(heredoc_tag)
                    if end_pos < len(text) and text[end_pos] in (';\n', ';', '\n'):
                        in_heredoc = False
                        heredoc_tag = None
                        i = end_pos
                        continue
            i += 1
            continue

        # Handle string escapes
        if in_single_quote:
            if ch == '\\' and i + 1 < len(text):
                i += 2
                continue
            if ch == "'":
                in_single_quote = False
            i += 1
            continue

        if in_double_quote:
            if ch == '\\' and i + 1 < len(text):
                i += 2
                continue
            if ch == '"':
                in_double_quote = False
            i += 1
            continue

        # Detect string starts
        if ch == "'":
            in_single_quote = True
            i += 1
            continue
        if ch == '"':
            in_double_quote = True
            i += 1
            continue

        # Detect heredoc/nowdoc
        if ch == '<' and text[i:i+3] == '<<<':
            rest = text[i+3:]
            m = re.match(r"'?(\w+)'?\n", rest)
            if m:
                heredoc_tag = m.group(1)
                in_heredoc = True
                i += 3 + len(m.group(0))
                continue

        # Line comments
        if ch == '/' and i + 1 < len(text) and text[i+1] == '/':
            nl = text.find('\n', i)
            if nl < 0:
                break
            i = nl + 1
            continue

        # Block comments
        if ch == '/' and i + 1 < len(text) and text[i+1] == '*':
            end = text.find('*/', i + 2)
            if end < 0:
                break
            i = end + 2
            continue

        # Braces
        if ch == '{':
            depth += 1
        elif ch == '}':
            depth -= 1
            if depth == 0:
                return i

        i += 1

    return -1


def parse_php_items(content, file_path, item_filter=None):
    """Parse PHP methods and standalone functions from source content.

    Returns a list of parsed items with boundaries.
    If item_filter is provided, only return items matching those names.
    """
    items = []
    lines = content.split('\n')

    # Match class methods and standalone functions
    # Handles: public function, protected function, private function,
    # public static function, abstract function, final function, etc.
    pattern = re.compile(
        r'(?:(?:public|protected|private|static|abstract|final)\s+)*'
        r'function\s+(\w+)\s*\([^)]*\)(?:\s*:\s*[\w\\|?]+)?\s*\{',
        re.MULTILINE
    )

    for m in pattern.finditer(content):
        name = m.group(1)

        # Skip test methods
        if name.startswith('test_') or name.startswith('test'):
            continue

        if item_filter and name not in item_filter:
            continue

        # Find the opening brace position
        brace_pos = m.end() - 1  # The { at the end of the match
        end_brace = find_matching_brace(content, brace_pos)
        if end_brace < 0:
            continue

        # Find start line (include doc comment if present)
        match_start = m.start()

        # Look backwards for doc comment
        before = content[:match_start].rstrip()
        if before.endswith('*/'):
            doc_start = before.rfind('/**')
            if doc_start >= 0:
                match_start = doc_start

        # Convert positions to line numbers (1-indexed)
        start_line = content[:match_start].count('\n') + 1
        end_line = content[:end_brace + 1].count('\n') + 1

        # Extract source
        source_lines = lines[start_line - 1:end_line]
        source = '\n'.join(source_lines)

        # Detect visibility
        vis = ''
        vis_match = re.search(r'\b(public|protected|private)\b', m.group(0))
        if vis_match:
            vis = vis_match.group(1)

        items.append({
            'name': name,
            'kind': 'method',
            'start_line': start_line,
            'end_line': end_line,
            'source': source,
            'visibility': vis,
        })

    return items


def detect_namespace(content):
    """Extract the PHP namespace from file content."""
    m = re.search(r'namespace\s+([\w\\]+)\s*;', content)
    return m.group(1) if m else None


def detect_class_name(content):
    """Extract the primary class name from file content."""
    m = re.search(r'class\s+(\w+)', content)
    return m.group(1) if m else None


def namespace_to_path(namespace):
    """Convert a PHP namespace to a directory path.

    DataMachine\\Abilities\\Traits -> inc/Abilities/Traits
    The root namespace (DataMachine) maps to inc/.
    """
    parts = namespace.split('\\')
    if parts and parts[0] == 'DataMachine':
        parts[0] = 'inc'
    return '/'.join(parts)


def path_to_namespace(file_path, root_mapping='inc:DataMachine'):
    """Convert a file path to a PHP namespace.

    inc/Abilities/Traits/HasPermissionCheck.php -> DataMachine\\Abilities\\Traits
    """
    root_dir, root_ns = root_mapping.split(':')
    # Strip the file extension
    path = re.sub(r'\.php$', '', file_path)
    # Get directory (namespace comes from dir, not filename)
    parts = path.split('/')
    dir_parts = parts[:-1]

    if dir_parts and dir_parts[0] == root_dir:
        dir_parts[0] = root_ns

    return '\\'.join(dir_parts)


def function_name_to_trait_name(function_name):
    """Convert a function name to a trait name.

    checkPermission -> HasCheckPermission
    __construct -> HasSharedConstructor
    register -> HasRegister
    get_config -> HasGetConfig
    httpGet -> HasHttpGet
    """
    if function_name == '__construct':
        return 'HasSharedConstructor'

    # If already camelCase/PascalCase, just uppercase the first letter
    if '_' not in function_name:
        return f'Has{function_name[0].upper()}{function_name[1:]}'

    # Convert snake_case to PascalCase
    parts = function_name.split('_')
    pascal = ''.join(p.capitalize() for p in parts if p)

    return f'Has{pascal}'


def extract_method_dependencies(method_source, canonical_content):
    """Find use/import statements from the canonical file that the method needs.

    Scans the method body for class references and matches them against
    the canonical file's use statements.
    """
    # Get all use statements from canonical file
    use_stmts = {}
    for m in re.finditer(r'^use\s+([\w\\]+)(?:\s+as\s+(\w+))?;', canonical_content, re.MULTILINE):
        fqn = m.group(1)
        alias = m.group(2)
        short_name = alias or fqn.split('\\')[-1]
        use_stmts[short_name] = m.group(0)

    # Find class references in the method source
    needed = []
    for short_name, stmt in use_stmts.items():
        # Check if the short name appears in the method body
        if re.search(r'\b' + re.escape(short_name) + r'\b', method_source):
            needed.append(stmt)

    return needed


def generate_trait_file(function_name, method_source, namespace_base, trait_name,
                        dependency_imports=None):
    """Generate a complete PHP trait file.

    Args:
        function_name: Name of the duplicated function
        method_source: The full method source code (with doc comment)
        namespace_base: Base namespace for the trait (e.g., DataMachine\\Abilities)
        trait_name: Name of the trait
        dependency_imports: List of use statements the method depends on
    """
    trait_namespace = f'{namespace_base}\\Traits'

    lines = [
        '<?php',
        '',
        f'namespace {trait_namespace};',
    ]

    # Add dependency imports if any
    if dependency_imports:
        lines.append('')
        for imp in sorted(dependency_imports):
            lines.append(imp)

    lines.extend([
        '',
        f'/**',
        f' * Shared trait for the `{function_name}` method.',
        f' *',
        f' * Extracted by homeboy audit --fix from duplicate implementations.',
        f' */',
        f'trait {trait_name} {{',
    ])

    # The method source comes from inside a class, so it already has one level
    # of indentation (typically a tab). Inside a trait, it needs the same
    # single level of indentation. Preserve the original indentation as-is.
    source_lines = method_source.split('\n')

    # Detect the indentation style of the source (tabs or spaces)
    indent_char = '\t'
    for sl in source_lines:
        if sl and sl[0] in (' ', '\t'):
            indent_char = sl[0]
            break

    for line in source_lines:
        rstripped = line.rstrip()
        if rstripped:
            lines.append(rstripped)
        else:
            lines.append('')

    lines.append('}')
    lines.append('')

    return '\n'.join(lines)


def extract_shared(data):
    """Generate trait extraction plan for a group of duplicate functions.

    Input:
        function_name: str — the duplicated function
        canonical_file: str — file chosen to keep the original
        canonical_content: str — content of the canonical file
        files: list of {path, content} — all files containing the duplicate
        root_namespace_mapping: str — e.g., "inc:DataMachine"

    Output:
        trait_file: str — path for the new trait file
        trait_content: str — full content of the trait file
        file_edits: list of {file, remove_lines, add_use_trait, add_import}
    """
    function_name = data['function_name']
    canonical_file = data['canonical_file']
    canonical_content = data['canonical_content']
    files = data.get('files', [])
    root_mapping = data.get('root_namespace_mapping', 'inc:DataMachine')

    # Parse the canonical file to get the method source
    items = parse_php_items(canonical_content, canonical_file, item_filter=[function_name])
    if not items:
        return {'error': f'Function {function_name} not found in canonical file {canonical_file}'}

    item = items[0]
    method_source = item['source']

    # Find imports the method depends on
    dependency_imports = extract_method_dependencies(method_source, canonical_content)

    # Detect namespace of canonical file to determine trait namespace
    canonical_ns = detect_namespace(canonical_content)
    if not canonical_ns:
        # Fall back to path-based namespace
        canonical_ns = path_to_namespace(canonical_file, root_mapping)

    # Determine base namespace for the trait
    # Use the common namespace prefix of all files, or the canonical's namespace
    ns_parts = canonical_ns.split('\\')
    # Go up one level for the trait (e.g., DataMachine\Abilities -> DataMachine\Abilities\Traits)
    trait_namespace_base = canonical_ns

    trait_name = function_name_to_trait_name(function_name)
    trait_namespace = f'{trait_namespace_base}\\Traits'
    trait_file_path = namespace_to_path(trait_namespace) + f'/{trait_name}.php'

    # Generate the trait file content
    trait_content = generate_trait_file(
        function_name, method_source, trait_namespace_base, trait_name,
        dependency_imports=dependency_imports,
    )

    # Generate edit instructions for each file
    file_edits = []
    all_files = [{'path': canonical_file, 'content': canonical_content}] + files

    for file_info in all_files:
        fpath = file_info['path']
        fcontent = file_info['content']

        # Parse to find function boundaries in this file
        file_items = parse_php_items(fcontent, fpath, item_filter=[function_name])
        if not file_items:
            continue

        fi = file_items[0]

        # Build the import statement
        fqn = f'{trait_namespace}\\{trait_name}'
        import_stmt = f'use {fqn};'

        # Build the use-inside-class statement
        use_trait_stmt = f'    use {trait_name};'

        # Check if file already has this import
        has_import = fqn in fcontent or import_stmt in fcontent

        # Check if file already uses this trait inside the class
        has_use_trait = f'use {trait_name};' in fcontent

        edit = {
            'file': fpath,
            'remove_lines': {
                'start_line': fi['start_line'],
                'end_line': fi['end_line'],
            },
        }

        if not has_import:
            edit['add_import'] = import_stmt

        if not has_use_trait:
            edit['add_use_trait'] = use_trait_stmt

        file_edits.append(edit)

    return {
        'trait_file': trait_file_path,
        'trait_content': trait_content,
        'trait_name': trait_name,
        'trait_namespace': trait_namespace,
        'file_edits': file_edits,
    }


# ============================================================================
# Command Dispatch
# ============================================================================

def main():
    data = json.load(sys.stdin)
    command = data.get('command', '')

    if command == 'parse_items':
        content = data.get('content', '')
        file_path = data.get('file_path', '')
        item_filter = data.get('items', None)
        items = parse_php_items(content, file_path, item_filter=item_filter)
        print(json.dumps({'items': items}))

    elif command == 'extract_shared':
        result = extract_shared(data)
        print(json.dumps(result))

    else:
        print(json.dumps({'error': f'Unknown command: {command}'}))
        sys.exit(1)


if __name__ == '__main__':
    main()
