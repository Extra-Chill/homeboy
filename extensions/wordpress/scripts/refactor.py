#!/usr/bin/env python3
"""PHP refactor script — language-specific parsing for homeboy refactor & audit fix.

Receives JSON commands on stdin, outputs JSON results on stdout.

Commands:
  parse_items      — Parse class methods/functions in a PHP source file
  extract_shared   — Generate a shared trait file + usage instructions for duplicates
"""

import json
import os
import re
import shlex
import sys
import tempfile


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


# ============================================================================
# PSR-4 Autoloader Resolution
# ============================================================================

def load_psr4_mappings(project_root):
    """Load PSR-4 namespace-to-directory mappings from composer.json.

    Returns a dict of {namespace_prefix: directory_path} sorted by
    specificity (longest prefix first) for correct matching.
    """
    if not project_root:
        return {}

    composer_path = os.path.join(project_root, 'composer.json')
    if not os.path.isfile(composer_path):
        return {}

    try:
        with open(composer_path, 'r') as f:
            composer = json.load(f)
    except (json.JSONDecodeError, OSError):
        return {}

    mappings = {}

    # Read autoload.psr-4
    psr4 = composer.get('autoload', {}).get('psr-4', {})
    for ns_prefix, dir_path in psr4.items():
        # Normalize: strip trailing backslash from namespace
        ns_prefix = ns_prefix.rstrip('\\')
        # Handle array of directories (take first)
        if isinstance(dir_path, list):
            dir_path = dir_path[0] if dir_path else ''
        # Normalize: strip trailing slash from directory
        dir_path = dir_path.rstrip('/')
        if ns_prefix and dir_path:
            mappings[ns_prefix] = dir_path

    # Also check autoload-dev.psr-4
    psr4_dev = composer.get('autoload-dev', {}).get('psr-4', {})
    for ns_prefix, dir_path in psr4_dev.items():
        ns_prefix = ns_prefix.rstrip('\\')
        if isinstance(dir_path, list):
            dir_path = dir_path[0] if dir_path else ''
        dir_path = dir_path.rstrip('/')
        if ns_prefix and dir_path:
            mappings[ns_prefix] = dir_path

    return mappings


def namespace_to_path(namespace, psr4_mappings=None):
    """Convert a PHP namespace to a directory path using PSR-4 mappings.

    With PSR-4 mappings from composer.json:
        ExamplePlugin\\Abilities\\Traits -> inc/Abilities/Traits
        (if composer.json has "ExamplePlugin\\": "inc/")
    """
    if psr4_mappings:
        # Sort by specificity — longest namespace prefix first
        sorted_mappings = sorted(
            psr4_mappings.items(),
            key=lambda x: x[0].count('\\'),
            reverse=True,
        )

        for ns_prefix, dir_path in sorted_mappings:
            # Check if the namespace starts with this PSR-4 prefix
            if namespace == ns_prefix:
                return dir_path
            if namespace.startswith(ns_prefix + '\\'):
                remainder = namespace[len(ns_prefix) + 1:]
                return dir_path + '/' + remainder.replace('\\', '/')

    return namespace.replace('\\', '/')


def path_to_namespace(file_path, psr4_mappings=None, root_mapping='inc:ExamplePlugin'):
    """Convert a file path to a PHP namespace using PSR-4 mappings.

    inc/Abilities/Traits/HasPermissionCheck.php -> ExamplePlugin\\Abilities\\Traits
    """
    # Strip the file extension
    path = re.sub(r'\.php$', '', file_path)
    # Get directory (namespace comes from dir, not filename)
    parts = path.split('/')
    dir_parts = parts[:-1]
    dir_path = '/'.join(dir_parts)

    if psr4_mappings:
        # Sort by specificity — longest directory path first
        sorted_mappings = sorted(
            psr4_mappings.items(),
            key=lambda x: x[1].count('/'),
            reverse=True,
        )

        for ns_prefix, mapped_dir in sorted_mappings:
            if dir_path == mapped_dir:
                return ns_prefix
            if dir_path.startswith(mapped_dir + '/'):
                remainder = dir_path[len(mapped_dir) + 1:]
                return ns_prefix + '\\' + remainder.replace('/', '\\')

    # Fallback: use root_mapping
    root_dir, root_ns = root_mapping.split(':')
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


def normalize_method_body(source):
    """Normalize a method body for comparison.

    Strips doc comments, collapses whitespace, and normalizes indentation
    so that semantically identical methods compare equal regardless of
    formatting differences.
    """
    lines = source.split('\n')
    result_lines = []
    in_doc = False

    for line in lines:
        stripped = line.strip()

        # Skip doc comments
        if stripped.startswith('/**'):
            in_doc = True
            continue
        if in_doc:
            if stripped.endswith('*/'):
                in_doc = False
            continue

        # Skip single-line comments
        if stripped.startswith('//'):
            continue

        # Skip empty lines
        if not stripped:
            continue

        result_lines.append(stripped)

    return ' '.join(result_lines)


def methods_are_identical(source_a, source_b):
    """Compare two method sources to determine if they are semantically identical.

    Normalizes both sources (strips doc comments, whitespace) before comparison.
    Returns True if the method bodies are the same.
    """
    return normalize_method_body(source_a) == normalize_method_body(source_b)


def generate_trait_file(function_name, method_source, namespace_base, trait_name,
                        dependency_imports=None):
    """Generate a complete PHP trait file.

    Args:
        function_name: Name of the duplicated function
        method_source: The full method source code (with doc comment)
        namespace_base: Base namespace for the trait (e.g., ExamplePlugin\\Abilities)
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


def common_namespace_prefix(namespaces):
    """Find the longest common namespace prefix from a list of namespaces.

    ['ExamplePlugin\\Abilities\\Flow', 'ExamplePlugin\\Abilities\\Job',
     'ExamplePlugin\\Abilities\\Taxonomy']
    → 'ExamplePlugin\\Abilities'
    """
    if not namespaces:
        return ''
    parts_list = [ns.split('\\') for ns in namespaces]
    prefix = []
    for segments in zip(*parts_list):
        if len(set(segments)) == 1:
            prefix.append(segments[0])
        else:
            break
    return '\\'.join(prefix)


def extract_shared(data):
    """Generate trait extraction plan for a group of duplicate functions.

    Input:
        function_name: str — the duplicated function
        canonical_file: str — file chosen to keep the original
        canonical_content: str — content of the canonical file
        files: list of {path, content} — all files containing the duplicate
        all_file_paths: list of str — all file paths in the group
        project_root: str — absolute path to the project root (for composer.json)

    Output:
        trait_file: str — path for the new trait file
        trait_content: str — full content of the trait file
        file_edits: list of {file, remove_lines, add_use_trait, add_import}
        skipped_files: list of {file, reason} — files skipped due to body mismatch
    """
    function_name = data['function_name']
    canonical_file = data['canonical_file']
    canonical_content = data['canonical_content']
    files = data.get('files', [])
    all_file_paths = data.get('all_file_paths', [canonical_file])
    project_root = data.get('project_root', '')

    # Load PSR-4 mappings from composer.json
    psr4_mappings = load_psr4_mappings(project_root)

    # Parse the canonical file to get the method source
    items = parse_php_items(canonical_content, canonical_file, item_filter=[function_name])
    if not items:
        return {'error': f'Function {function_name} not found in canonical file {canonical_file}'}

    item = items[0]
    method_source = item['source']

    # Find imports the method depends on
    dependency_imports = extract_method_dependencies(method_source, canonical_content)

    # Detect namespaces from all files to find the common ancestor
    all_contents = {canonical_file: canonical_content}
    for f in files:
        all_contents[f['path']] = f['content']

    namespaces = []
    for fpath in all_file_paths:
        if fpath in all_contents:
            ns = detect_namespace(all_contents[fpath])
            if ns:
                namespaces.append(ns)

    # Fall back to canonical namespace if we can't read all files
    if not namespaces:
        canonical_ns = detect_namespace(canonical_content)
        if canonical_ns:
            namespaces = [canonical_ns]

    if not namespaces:
        return {'error': f'Cannot determine namespace for {function_name}'}

    # Compute the common ancestor namespace for trait placement
    trait_namespace_base = common_namespace_prefix(namespaces)

    # If common prefix is too short (just the root namespace), use canonical's namespace
    if trait_namespace_base.count('\\') < 1:
        trait_namespace_base = namespaces[0]

    trait_name = function_name_to_trait_name(function_name)
    trait_namespace = f'{trait_namespace_base}\\Traits'
    trait_file_path = namespace_to_path(trait_namespace, psr4_mappings) + f'/{trait_name}.php'

    # Generate the trait file content
    trait_content = generate_trait_file(
        function_name, method_source, trait_namespace_base, trait_name,
        dependency_imports=dependency_imports,
    )

    # Generate edit instructions for each file, verifying body matches
    file_edits = []
    skipped_files = []
    all_files = [{'path': canonical_file, 'content': canonical_content}] + files

    for file_info in all_files:
        fpath = file_info['path']
        fcontent = file_info['content']

        # Parse to find function boundaries in this file
        file_items = parse_php_items(fcontent, fpath, item_filter=[function_name])
        if not file_items:
            continue

        fi = file_items[0]

        # Verify the method body in this file actually matches the canonical
        # method body before generating edit instructions. Methods with the
        # same name but different implementations should not be extracted
        # into a shared trait.
        if fpath != canonical_file:
            if not methods_are_identical(method_source, fi['source']):
                skipped_files.append({
                    'file': fpath,
                    'reason': (
                        f'Method body differs from canonical — '
                        f'`{function_name}` in {fpath} has a different '
                        f'implementation than {canonical_file}'
                    ),
                })
                continue

        # Build the import statement
        fqn = f'{trait_namespace}\\{trait_name}'
        import_stmt = f'use {fqn};'

        # Build the use-inside-class statement (detect indentation from file)
        indent = '\t'  # default to tab
        for line in fcontent.split('\n'):
            stripped = line.lstrip()
            if stripped.startswith('public ') or stripped.startswith('private ') or stripped.startswith('protected '):
                indent = line[:len(line) - len(stripped)]
                break
        use_trait_stmt = f'{indent}use {trait_name};'

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

    # If all non-canonical files were skipped due to body mismatch,
    # the trait extraction is not useful — skip entirely.
    non_canonical_edits = [e for e in file_edits if e['file'] != canonical_file]
    if not non_canonical_edits and skipped_files:
        return {
            'skip': True,
            'reason': (
                f'All files have different implementations of `{function_name}` '
                f'— cannot extract shared trait'
            ),
            'skipped_files': skipped_files,
        }

    result = {
        'trait_file': trait_file_path,
        'trait_content': trait_content,
        'trait_name': trait_name,
        'trait_namespace': trait_namespace,
        'file_edits': file_edits,
    }

    if skipped_files:
        result['skipped_files'] = skipped_files

    return result


def refactor_source(data):
    """Handle Homeboy's generic extension refactor source command."""
    return {'handled': False}

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

    elif command == 'refactor_source':
        result = refactor_source(data)
        if result.get('fatal_error'):
            print(result['fatal_error'], file=sys.stderr)
            sys.exit(1)
        print(json.dumps(result))

    else:
        print(json.dumps({'error': f'Unknown command: {command}'}))
        sys.exit(1)


if __name__ == '__main__':
    main()
