#!/usr/bin/env bash
# WordPress/PHP fingerprint script for homeboy audit.
#
# Input (JSON on stdin):
#   {"file_path": "inc/Abilities/Foo.php", "content": "..."}
#
# Output (JSON on stdout):
#   {"methods": [...], "type_name": "...", "implements": [...],
#    "registrations": [], "namespace": "...", "imports": [...]}
#
# Extracts structural information from PHP source files using text matching.

set -euo pipefail

INPUT=$(cat)

printf '%s' "$INPUT" | python3 -c "
import json, sys, re

data = json.load(sys.stdin)
content = data['content']
file_path = data['file_path']

# --- Methods + Visibility ---
# Match PHP method declarations with visibility tracking
methods = []
visibility = {}
for m in re.finditer(
    r'((?:public|protected|private|static|abstract|final)\s+(?:(?:static|abstract|final)\s+)*)function\s+(\w+)',
    content
):
    modifiers = m.group(1).strip()
    name = m.group(2)
    # Skip test methods
    if name.startswith('test_') or name.startswith('test'):
        continue
    methods.append(name)
    # Determine visibility
    if 'private' in modifiers:
        visibility[name] = 'private'
    elif 'protected' in modifiers:
        visibility[name] = 'protected'
    else:
        visibility[name] = 'public'

# Also match standalone function declarations (not in a class)
for m in re.finditer(r'^function\s+(\w+)', content, re.MULTILINE):
    methods.append(m.group(1))
    visibility[m.group(1)] = 'public'

# Deduplicate preserving order
seen = set()
methods = [m for m in methods if m not in seen and not seen.add(m)]

# --- Type name + kind ---
# Primary class, interface, or trait in the file.
# Anchor to start-of-line to avoid matching 'class' in comments/strings.
# type_kind distinguishes class/interface/trait so core can skip traits
# from interface convention expectations (#115).
type_name = None
type_kind = None
for kind, pattern in [
    ('class', r'^(?:abstract\s+|final\s+)?class\s+(\w+)'),
    ('interface', r'^interface\s+(\w+)'),
    ('trait', r'^trait\s+(\w+)'),
]:
    match = re.search(pattern, content, re.MULTILINE)
    if match:
        type_name = match.group(1)
        type_kind = kind
        break

# --- Extends ---
# Extract the parent class separately (anchored to actual declaration)
extends = None
ext_match = re.search(r'^(?:abstract\s+|final\s+)?class\s+\w+\s+extends\s+([\w\\\\]+)', content, re.MULTILINE)
if ext_match:
    extends = ext_match.group(1).split('\\\\')[-1]

# --- Implements ---
# Interfaces and traits (NOT extends — that's separate now)
implements = []

# implements (can be comma-separated, on a class/interface declaration line)
impl_match = re.search(r'^(?:abstract\s+|final\s+)?(?:class|interface)\s+\w+(?:\s+extends\s+[\w\\\\]+)?\s+implements\s+([\w\\\\,\s]+?)(?:\s*\{)', content, re.MULTILINE)
if impl_match:
    for iface in impl_match.group(1).split(','):
        iface = iface.strip()
        if iface:
            implements.append(iface.split('\\\\')[-1])

# use Trait inside class body
for m in re.finditer(r'^\s+use\s+([\w\\\\]+)\s*[;{]', content, re.MULTILINE):
    trait_name = m.group(1).split('\\\\')[-1]
    implements.append(trait_name)

# Deduplicate
seen = set()
implements = [i for i in implements if i not in seen and not seen.add(i)]

# --- Properties ---
# Extract class properties (public/protected with type hints)
# Use \\$ (backslash + chr(36)) because chr(36) alone is regex end-of-string anchor
dollar_esc = '\\\\' + chr(36)  # produces \$ in the regex
dollar_lit = chr(36)  # produces $ for output strings
properties = []
for m in re.finditer(
    r'(public|protected|private)\s+(?:static\s+)?(?:readonly\s+)?(?:([\w\\\\|?]+)\s+)?' + dollar_esc + r'(\w+)',
    content
):
    prop_vis = m.group(1)
    prop_type = m.group(2) or ''
    prop_name = m.group(3)
    # Only include public/protected (the API surface)
    if prop_vis in ('public', 'protected'):
        if prop_type:
            properties.append(prop_type + ' ' + dollar_lit + prop_name)
        else:
            properties.append(dollar_lit + prop_name)

# Deduplicate
seen = set()
properties = [p for p in properties if p not in seen and not seen.add(p)]

# --- Hooks ---
# Extract do_action() and apply_filters() calls
hooks = []
# do_action( 'hook_name', ... ) and do_action_ref_array
for m in re.finditer(r'do_action(?:_ref_array)?\s*\(\s*[\x27\x22]([^\x27\x22]+)[\x27\x22]', content):
    hooks.append({'type': 'action', 'name': m.group(1)})
# apply_filters( 'hook_name', ... ) and apply_filters_ref_array
for m in re.finditer(r'apply_filters(?:_ref_array)?\s*\(\s*[\x27\x22]([^\x27\x22]+)[\x27\x22]', content):
    hooks.append({'type': 'filter', 'name': m.group(1)})
# Deduplicate by (type, name)
seen_hooks = set()
hooks = [h for h in hooks if (h['type'], h['name']) not in seen_hooks and not seen_hooks.add((h['type'], h['name']))]

# --- Registrations ---
# Match WordPress registration function calls
registrations = []
reg_patterns = [
    r'register_post_type\s*\(\s*[\'\"]([\w-]+)',
    r'register_taxonomy\s*\(\s*[\'\"]([\w-]+)',
    r'register_block_type\s*\(\s*[\'\"]([\w/-]+)',
    r'register_setting\s*\(\s*[\'\"]([\w-]+)',
    r'register_rest_route\s*\([^,]+,\s*[\'\"]([^\'\"]+)',
    r'add_shortcode\s*\(\s*[\'\"]([\w-]+)',
    r'add_action\s*\(\s*[\'\"]([\w-]+)',
    r'add_filter\s*\(\s*[\'\"]([\w-]+)',
    r'wp_register_script\s*\(\s*[\'\"]([\w-]+)',
    r'wp_register_style\s*\(\s*[\'\"]([\w-]+)',
    r'wp_register_ability\s*\(\s*[\'\"]([^\'\"]+)',
    r'wp_register_ability_category\s*\(\s*[\'\"]([^\'\"]+)',
    r'WP_CLI::add_command\s*\(\s*[\'\"]([^\'\"]+)',
    r'\\\$this->registerTool\s*\(\s*[\'\"][^\'\"]+[\'\"]\\s*,\\s*[\'\"]([^\'\"]+)',
]
for pat in reg_patterns:
    for m in re.finditer(pat, content):
        registrations.append(m.group(1))
# Deduplicate
seen = set()
registrations = [r for r in registrations if r not in seen and not seen.add(r)]

# --- Hook Callbacks (#118) ---
# Extract functions/methods registered as WordPress hook callbacks.
# These are externally invoked by WordPress core, not directly referenced.
# Note: use dollar_esc for regex matching of literal PHP dollar signs.
hook_callbacks = set()
this_pat = dollar_esc + r'this'
# add_action/add_filter with array( this, 'method' )
for m in re.finditer(
    r'(?:add_action|add_filter)\s*\([^,]+,\s*array\s*\(\s*' + this_pat + r'\s*,\s*[\x27\x22](\w+)[\x27\x22]\s*\)',
    content
):
    hook_callbacks.add(m.group(1))
# add_action/add_filter with [ this, 'method' ]
for m in re.finditer(
    r'(?:add_action|add_filter)\s*\([^,]+,\s*\[\s*' + this_pat + r'\s*,\s*[\x27\x22](\w+)[\x27\x22]\s*\]',
    content
):
    hook_callbacks.add(m.group(1))
# add_action/add_filter with string callback: 'function_name'
for m in re.finditer(
    r'(?:add_action|add_filter)\s*\([^,]+,\s*[\x27\x22](\w+)[\x27\x22]\s*[,)]',
    content
):
    hook_callbacks.add(m.group(1))
# add_action/add_filter with __CLASS__/self::class/static::class
for m in re.finditer(
    r'(?:add_action|add_filter)\s*\([^,]+,\s*(?:array\s*\(|[\[])\s*(?:__CLASS__|self::class|static::class)\s*,\s*[\x27\x22](\w+)[\x27\x22]',
    content
):
    hook_callbacks.add(m.group(1))
# register_activation_hook / register_deactivation_hook
for m in re.finditer(
    r'register_(?:activation|deactivation)_hook\s*\([^,]+,\s*[\x27\x22](\w+)[\x27\x22]',
    content
):
    hook_callbacks.add(m.group(1))
# Ability execute_callback and permission_callback arrays
for m in re.finditer(
    r'[\x27\x22](?:execute_callback|permission_callback)[\x27\x22]\s*=>\s*(?:array\s*\(|[\[])\s*' + this_pat + r'\s*,\s*[\x27\x22](\w+)[\x27\x22]',
    content
):
    hook_callbacks.add(m.group(1))
hook_callbacks = sorted(hook_callbacks)

# --- Namespace ---
# Match PHP namespace declaration
ns_match = re.search(r'namespace\s+([\w\\\\]+)\s*;', content)
if ns_match:
    namespace = ns_match.group(1)
else:
    namespace = None

# --- Imports ---
# Match PHP use statements (at file/namespace level, not trait use inside class)
imports = []
for m in re.finditer(r'^use\s+([\w\\\\]+)(?:\s+as\s+\w+)?;', content, re.MULTILINE):
    fqcn = m.group(1)
    short_name = fqcn.split('\\\\')[-1]
    # Skip self-imports: a file defining Foo doesn't need 'use Foo' (#117)
    if type_name and short_name == type_name:
        continue
    imports.append(fqcn)
# Deduplicate
seen = set()
imports = [i for i in imports if i not in seen and not seen.add(i)]

# Global namespace classes never need a use statement in PHP (#117).
# Instead of hardcoding a list, we track which class names in extends/implements
# are unqualified (no backslash = global namespace or same namespace).
# Core can use this to skip missing_import findings for these names.
# An unqualified name like 'WP_UnitTestCase' is auto-resolved by PHP —
# it's either in the current namespace or the global namespace.
uses_global_classes = []
if extends and '\\\\' not in extends:
    uses_global_classes.append(extends)
for iface in implements:
    if '\\\\' not in iface:
        uses_global_classes.append(iface)
# Deduplicate
seen = set()
uses_global_classes = [c for c in uses_global_classes if c not in seen and not seen.add(c)]

# --- Method Hashes (for duplication detection) ---
# Extract method/function bodies, normalize whitespace, hash with SHA-256.
import hashlib

method_hashes = {}
structural_hashes = {}

def extract_body(text, start_pos):
    # Find opening brace from start_pos, then track brace depth
    brace_start = text.find('{', start_pos)
    if brace_start < 0:
        return None
    depth = 0
    for i in range(brace_start, len(text)):
        if text[i] == '{':
            depth += 1
        elif text[i] == '}':
            depth -= 1
            if depth == 0:
                return text[brace_start:i+1]
    return None

php_keywords = {
    'abstract', 'and', 'array', 'as', 'break', 'callable', 'case',
    'catch', 'class', 'clone', 'const', 'continue', 'declare', 'default',
    'do', 'echo', 'else', 'elseif', 'empty', 'enddeclare', 'endfor',
    'endforeach', 'endif', 'endswitch', 'endwhile', 'eval', 'exit',
    'extends', 'final', 'finally', 'fn', 'for', 'foreach', 'function',
    'global', 'goto', 'if', 'implements', 'include', 'include_once',
    'instanceof', 'insteadof', 'interface', 'isset', 'list', 'match',
    'namespace', 'new', 'or', 'print', 'private', 'protected', 'public',
    'readonly', 'require', 'require_once', 'return', 'static', 'switch',
    'throw', 'trait', 'try', 'unset', 'use', 'var', 'while', 'xor',
    'yield', 'null', 'true', 'false', 'self', 'parent',
    # Common types kept as markers
    'int', 'float', 'string', 'bool', 'void', 'mixed', 'object',
    'iterable', 'never', 'array',
}

def structural_normalize_php(text):
    # Strip to body only (from first {)
    brace_idx = text.find('{')
    if brace_idx >= 0:
        text = text[brace_idx:]
    # Replace string literals with STR
    dq = chr(34)
    text = re.sub(dq + '[^' + dq + ']*' + dq, 'STR', text)
    text = re.sub(chr(39) + '[^' + chr(39) + ']*' + chr(39), 'STR', text)
    # Replace numeric literals with NUM
    text = re.sub(r'\b\d[\d_]*(?:\.\d[\d_]*)?\b', 'NUM', text)
    # Replace PHP variables with positional tokens
    var_map = {}
    var_counter = [0]
    def replace_var(m):
        name = m.group(0)
        if name == chr(36) + 'this':
            return name
        if name not in var_map:
            var_map[name] = 'VAR_' + str(var_counter[0])
            var_counter[0] += 1
        return var_map[name]
    text = re.sub(chr(36) + r'\w+', replace_var, text)
    # Replace non-keyword identifiers with positional tokens
    id_map = {}
    id_counter = [0]
    def replace_id(m):
        word = m.group(0)
        lower = word.lower()
        if lower in php_keywords:
            return word
        if word not in id_map:
            id_map[word] = 'ID_' + str(id_counter[0])
            id_counter[0] += 1
        return id_map[word]
    text = re.sub(r'\b[a-zA-Z_]\w*\b', replace_id, text)
    text = re.sub(r'\s+', ' ', text).strip()
    return text

# Extract method bodies from class methods
for m in re.finditer(
    r'(?:public|protected|private|static|abstract)\s+(?:static\s+)?function\s+(\w+)\s*\([^)]*\)(?:\s*:\s*[\w\\\\|?]+)?\s*',
    content
):
    fn_name = m.group(1)
    if fn_name.startswith('test_') or fn_name.startswith('test'):
        continue
    body = extract_body(content, m.end() - 1)
    if body and len(body) > 2:
        # Exact hash: normalize whitespace only
        normalized = re.sub(r'\s+', ' ', m.group(0) + body).strip()
        method_hashes[fn_name] = hashlib.sha256(normalized.encode()).hexdigest()[:16]
        # Structural hash: normalize identifiers and literals
        struct_text = m.group(0) + body
        struct_normalized = structural_normalize_php(struct_text)
        structural_hashes[fn_name] = hashlib.sha256(struct_normalized.encode()).hexdigest()[:16]

# Extract standalone function bodies (not in a class)
for m in re.finditer(r'^function\s+(\w+)\s*\([^)]*\)(?:\s*:\s*[\w\\\\|?]+)?\s*', content, re.MULTILINE):
    fn_name = m.group(1)
    if fn_name in method_hashes:
        continue
    body = extract_body(content, m.end() - 1)
    if body and len(body) > 2:
        normalized = re.sub(r'\s+', ' ', m.group(0) + body).strip()
        method_hashes[fn_name] = hashlib.sha256(normalized.encode()).hexdigest()[:16]
        struct_text = m.group(0) + body
        struct_normalized = structural_normalize_php(struct_text)
        structural_hashes[fn_name] = hashlib.sha256(struct_normalized.encode()).hexdigest()[:16]

# --- Public API ---
# Public methods exported from this file.
public_api = [m for m in methods if visibility.get(m) == 'public']

# --- Internal Calls ---
# Function/method calls within this file (for cross-file reference analysis).
internal_calls = set()
dollar = chr(36)
# Method calls: this->method( and self::method( and static::method( and ClassName::method(
for m in re.finditer(r'(?:' + dollar_esc + r'this->|self::|static::|[A-Z]\w*::)(\w+)\s*\(', content):
    name = m.group(1)
    if not name.startswith('test'):
        internal_calls.add(name)
# Free function calls: function_name(
for m in re.finditer(r'\b([a-z_]\w*)\s*\(', content):
    name = m.group(1)
    skip_php = {'if', 'while', 'for', 'foreach', 'switch', 'match', 'catch',
                'return', 'echo', 'print', 'isset', 'unset', 'empty', 'list',
                'array', 'function', 'class', 'interface', 'trait', 'new',
                'require', 'require_once', 'include', 'include_once',
                'define', 'defined', 'die', 'exit', 'eval', 'compact',
                'extract', 'var_dump', 'print_r', 'var_export'}
    if name not in skip_php and not name.startswith('test'):
        internal_calls.add(name)
internal_calls = sorted(internal_calls)

# --- Unused Parameters (#114) ---
# For each method/function, extract parameter names and check if they appear in the body.
# Skip contract-mandated params: hook callbacks, interface/abstract overrides,
# ability callbacks, and REST API handlers.
unused_parameters = []

# Build set of methods that are contract-mandated (their signature is fixed
# by the caller/interface, so unused params are expected).
contract_mandated_methods = set(hook_callbacks)  # hook callbacks already collected

# If a class extends or implements anything, its public methods may have
# signatures fixed by the parent/interface contract. We can't resolve the
# parent source statically, so we treat all public methods in such classes
# as potentially contract-mandated. This avoids false positives for
# interface implementations (DirectiveInterface, etc.) and abstract overrides.
if extends or implements:
    contract_mandated_methods.update(
        m for m in methods if visibility.get(m) == 'public'
    )

def check_unused_params(fn_name, params_str, body_text):
    # Skip entirely if this method is contract-mandated
    if fn_name in contract_mandated_methods:
        return
    # Parse parameter names from the signature
    param_names = []
    for pm in re.finditer(dollar_esc + r'(\w+)', params_str):
        pname = pm.group(1)
        if pname != 'this':
            param_names.append(pname)
    for pname in param_names:
        # Skip params prefixed with _ (intentionally unused)
        if pname.startswith('_'):
            continue
        # Check if the param variable appears in the body (after the signature)
        # Use dollar_esc (\\$) for regex to match literal $ not end-of-string
        if not re.search(dollar_esc + re.escape(pname) + r'\b', body_text):
            unused_parameters.append({'function': fn_name, 'param': pname})

# Class methods
for m in re.finditer(
    r'(?:public|protected|private|static|abstract)\s+(?:static\s+)?function\s+(\w+)\s*\(([^)]*)\)(?:\s*:\s*[\w\\\\|?]+)?\s*',
    content
):
    fn_name = m.group(1)
    if fn_name.startswith('test'):
        continue
    params_str = m.group(2)
    body = extract_body(content, m.end() - 1)
    if body and len(body) > 2:
        check_unused_params(fn_name, params_str, body)

# Standalone functions
for m in re.finditer(r'^function\s+(\w+)\s*\(([^)]*)\)(?:\s*:\s*[\w\\\\|?]+)?\s*', content, re.MULTILINE):
    fn_name = m.group(1)
    # Skip standalone functions registered as hook callbacks
    if fn_name in contract_mandated_methods:
        continue
    params_str = m.group(2)
    body = extract_body(content, m.end() - 1)
    if body and len(body) > 2:
        check_unused_params(fn_name, params_str, body)

# --- Dead Code Markers ---
# Find @codeCoverageIgnore, @phpstan-ignore, and similar suppression markers.
dead_code_markers = []
lines_arr = content.split('\n')
for line_num, line in enumerate(lines_arr, 1):
    stripped = line.strip()
    marker_type = None
    if '@codeCoverageIgnore' in stripped:
        marker_type = 'coverage_ignore'
    elif '@phpstan-ignore' in stripped:
        marker_type = 'phpstan_ignore'
    elif 'SuppressWarnings' in stripped:
        marker_type = 'suppress_warnings'
    if marker_type:
        # Find the next function/class/method declaration
        for k in range(line_num, min(line_num + 5, len(lines_arr))):
            next_line = lines_arr[k].strip()
            if next_line and not next_line.startswith('*') and not next_line.startswith('//') and not next_line.startswith('/*'):
                item_match = re.match(
                    r'(?:public|protected|private|static|abstract|final)?\s*(?:static\s+)?(?:function|class|interface|trait)\s+(\w+)',
                    next_line
                )
                if item_match:
                    dead_code_markers.append({
                        'item': item_match.group(1),
                        'line': line_num,
                        'marker_type': marker_type,
                    })
                break

result = {
    'methods': methods,
    'type_name': type_name,
    'type_kind': type_kind,
    'extends': extends,
    'implements': implements,
    'registrations': registrations,
    'namespace': namespace,
    'imports': imports,
    'method_hashes': method_hashes,
    'structural_hashes': structural_hashes,
    'visibility': visibility,
    'properties': properties,
    'hooks': hooks,
    'hook_callbacks': hook_callbacks,
    'unused_parameters': unused_parameters,
    'dead_code_markers': dead_code_markers,
    'internal_calls': internal_calls,
    'public_api': public_api,
    'uses_global_classes': uses_global_classes,
}

print(json.dumps(result))
"
