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

# --- Type name ---
# Primary class, interface, or trait in the file.
# Anchor to start-of-line to avoid matching 'class' in comments/strings.
type_name = None
for pattern in [
    r'^(?:abstract\s+|final\s+)?class\s+(\w+)',
    r'^interface\s+(\w+)',
    r'^trait\s+(\w+)',
]:
    match = re.search(pattern, content, re.MULTILINE)
    if match:
        type_name = match.group(1)
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
    imports.append(m.group(1))
# Deduplicate
seen = set()
imports = [i for i in imports if i not in seen and not seen.add(i)]

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

result = {
    'methods': methods,
    'type_name': type_name,
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
}

print(json.dumps(result))
"
