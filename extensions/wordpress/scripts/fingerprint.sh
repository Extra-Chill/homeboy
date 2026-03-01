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

# --- Methods ---
# Match PHP method declarations: public function name, protected function name,
# private function name, static function name, abstract function name
methods = []
for m in re.finditer(
    r'(?:public|protected|private|static|abstract)\s+(?:static\s+)?function\s+(\w+)',
    content
):
    name = m.group(1)
    # Skip test methods
    if name.startswith('test_') or name.startswith('test'):
        continue
    methods.append(name)

# Also match standalone function declarations (not in a class)
for m in re.finditer(r'^function\s+(\w+)', content, re.MULTILINE):
    methods.append(m.group(1))

# Deduplicate preserving order
seen = set()
methods = [m for m in methods if m not in seen and not seen.add(m)]

# --- Type name ---
# Primary class, interface, or trait in the file
type_name = None
# Prefer class, then interface, then trait
for pattern in [
    r'class\s+(\w+)',
    r'interface\s+(\w+)',
    r'trait\s+(\w+)',
]:
    match = re.search(pattern, content)
    if match:
        type_name = match.group(1)
        break

# --- Implements ---
# Match: class Foo extends Bar implements Baz, Qux
implements = []
# extends
ext_match = re.search(r'class\s+\w+\s+extends\s+([\w\\\\]+)', content)
if ext_match:
    implements.append(ext_match.group(1).split('\\\\')[-1])

# implements (can be comma-separated)
impl_match = re.search(r'implements\s+([\w\\\\,\s]+?)(?:\s*\{)', content)
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

result = {
    'methods': methods,
    'type_name': type_name,
    'implements': implements,
    'registrations': registrations,
    'namespace': namespace,
    'imports': imports,
}

print(json.dumps(result))
"
