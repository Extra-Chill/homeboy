#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FINGERPRINT="${SCRIPT_DIR}/../scripts/fingerprint.sh"
WORK_DIR="$(mktemp -d -t homeboy-rust-policy-flow.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

cat > "$WORK_DIR/fixture.rs" <<'RS'
pub enum Decision {
    Allow,
    Deny,
}

pub struct Policy {
    pub allowed: bool,
    pub blocked: bool,
    pub decision: Decision,
}

pub struct PolicyView {
    pub allowed: bool,
}

pub struct DecisionDto {
    pub decision: Decision,
}

impl Policy {
    pub fn authoritative(&self) -> Decision {
        if self.blocked {
            Decision::Deny
        } else if self.allowed {
            Decision::Allow
        } else {
            self.decision
        }
    }

    pub fn audit(&self) -> bool {
        self.allowed
    }

    pub fn set_allowed(&mut self) {
        self.allowed = true;
    }

    pub fn duplicate_branch(&self, decision: Decision) -> bool {
        if let Decision::Allow = decision {
            true
        } else {
            false
        }
    }
}

pub fn lossy(policy: &Policy) -> PolicyView {
    PolicyView {
        allowed: policy.allowed,
    }
}

pub fn shorthand(policy: &Policy) -> PolicyView {
    let allowed: bool = policy.allowed;
    PolicyView { allowed }
}

pub fn update(policy: Policy) -> Policy {
    Policy {
        blocked: false,
        ..policy
    }
}

pub fn ordinary_dto(policy: &Policy) -> DecisionDto {
    DecisionDto {
        decision: policy.decision,
    }
}

pub fn downstream(policy: &Policy) -> bool {
    match policy.authoritative() {
        Decision::Allow => true,
        Decision::Deny => false,
    }
}

pub fn duplicate_branch(decision: Decision) -> bool {
    if let Decision::Allow = decision {
        true
    } else {
        false
    }
}

pub fn delegate(policy: &Policy) -> Decision {
    policy.authoritative()
}

pub fn ordinary_call(policy: &Policy) {
    policy.audit();
}

pub fn unresolved(mystery: Mystery, policy: &Policy) {
    mystery.check();
    let unknown = mystery.value;
    UnknownView { unknown };
    policy.missing().unknown;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestOnlyPolicy {
        allowed: bool,
        blocked: bool,
        decision: Decision,
    }

    fn helper(policy: &Policy) -> PolicyView {
        if let Decision::Allow = policy.authoritative() {
            PolicyView { allowed: policy.allowed }
        } else {
            PolicyView { allowed: false }
        }
    }

    #[test]
    fn policy_flow_is_test_only() {
        let policy = Policy { allowed: true, blocked: false, decision: Decision::Allow };
        match policy.authoritative() {
            Decision::Allow => { helper(&policy); }
            Decision::Deny => {}
        }
    }
}
RS

make_input() {
    python3 - "$WORK_DIR/fixture.rs" <<'PY'
import json
import pathlib
import sys

print(json.dumps({
    "file_path": "src/policy/flow.rs",
    "content": pathlib.Path(sys.argv[1]).read_text(),
}))
PY
}

input="$(make_input)"
first="$(printf '%s' "$input" | "$FINGERPRINT")"
second="$(printf '%s' "$input" | "$FINGERPRINT")"

if [ "$first" != "$second" ]; then
    printf 'Fingerprint output changed across identical runs\n' >&2
    exit 1
fi

RESULT_JSON="$first" FINGERPRINT="$FINGERPRINT" python3 <<'PY'
import json
import os
import subprocess

result = json.loads(os.environ["RESULT_JSON"])
module = "crate::policy::flow"

for key in (
    "aggregate_definitions",
    "field_accesses",
    "aggregate_projections",
    "decision_branches",
    "method_calls",
):
    assert key in result and isinstance(result[key], list), key

definitions = {item["type_id"]: item for item in result["aggregate_definitions"]}
assert set(definitions) == {
    f"{module}::Policy",
    f"{module}::PolicyView",
    f"{module}::DecisionDto",
}
assert f"{module}::TestOnlyPolicy" not in definitions
assert [field["name"] for field in definitions[f"{module}::Policy"]["fields"]] == [
    "allowed",
    "blocked",
    "decision",
]
assert definitions[f"{module}::Policy"]["fields"][2]["type_id"] == f"{module}::Decision"

accesses = result["field_accesses"]
authoritative = f"{module}::Policy::authoritative"
assert {(item["field"], item["access"]) for item in accesses if item["callable_id"] == authoritative} == {
    ("allowed", "read"),
    ("blocked", "read"),
    ("decision", "read"),
}
assert all(item["owner_type_id"] == f"{module}::Policy" for item in accesses)
assert any(
    item["callable_id"] == f"{module}::Policy::set_allowed"
    and item["field"] == "allowed"
    and item["access"] == "write"
    for item in accesses
)

projections = result["aggregate_projections"]
projection_keys = {
    (item["callable_id"], item["source_type_id"], item["target_type_id"]): {
        (mapping["source_field"], mapping["target_field"])
        for mapping in item["field_mappings"]
    }
    for item in projections
}
assert projection_keys[(f"{module}::lossy", f"{module}::Policy", f"{module}::PolicyView")] == {
    ("allowed", "allowed")
}
assert projection_keys[(f"{module}::shorthand", f"{module}::Policy", f"{module}::PolicyView")] == {
    ("allowed", "allowed")
}
assert projection_keys[(f"{module}::ordinary_dto", f"{module}::Policy", f"{module}::DecisionDto")] == {
    ("decision", "decision")
}
assert projection_keys[(f"{module}::update", f"{module}::Policy", f"{module}::Policy")] == {
    ("allowed", "allowed"),
    ("decision", "decision"),
}
policy_fields = {field["name"] for field in definitions[f"{module}::Policy"]["fields"]}
update_fields = {
    target
    for _, target in projection_keys[(f"{module}::update", f"{module}::Policy", f"{module}::Policy")]
}
assert policy_fields - update_fields == {"blocked"}

allow_branch_callables = {
    item["callable_id"]
    for item in result["decision_branches"]
    if item["discriminant_id"] == f"{module}::Decision::Allow"
}
assert allow_branch_callables == {
    f"{module}::Policy::duplicate_branch",
    f"{module}::downstream",
    f"{module}::duplicate_branch",
}
assert all(item["domain_type_id"] == f"{module}::Decision" for item in result["decision_branches"])

calls = {(item["caller_id"], item["target_method_id"]): item for item in result["method_calls"]}
target = f"{module}::Policy::authoritative"
assert calls[(f"{module}::downstream", target)]["result_used_as_decision"] is True
assert calls[(f"{module}::downstream", target)]["decision_domain_type_id"] == f"{module}::Decision"
assert calls[(f"{module}::delegate", target)]["result_used_as_decision"] is True
assert calls[(f"{module}::delegate", target)]["decision_domain_type_id"] == f"{module}::Decision"
audit = calls[(f"{module}::ordinary_call", f"{module}::Policy::audit")]
assert audit["result_used_as_decision"] is False
assert "decision_domain_type_id" not in audit

serialized = json.dumps(result, sort_keys=True)
assert "Mystery" not in serialized
assert "UnknownView" not in serialized
policy_flow_serialized = json.dumps({
    key: result[key]
    for key in (
        "aggregate_definitions",
        "field_accesses",
        "aggregate_projections",
        "decision_branches",
        "method_calls",
    )
}, sort_keys=True)
assert "helper" not in policy_flow_serialized
assert "policy_flow_is_test_only" not in policy_flow_serialized
for collection in result.values():
    if isinstance(collection, list) and collection and isinstance(collection[0], dict):
        for item in collection:
            location = item.get("location")
            if location:
                assert location["line"] >= 1 and location["column"] >= 1

assert result["aggregate_definitions"] == sorted(
    result["aggregate_definitions"], key=lambda item: (item["type_id"], item["location"]["line"], item["location"]["column"])
)

def fingerprint(path, content):
    completed = subprocess.run(
        [os.environ["FINGERPRINT"]],
        input=json.dumps({"file_path": path, "content": content}),
        text=True,
        capture_output=True,
        check=True,
    )
    return json.loads(completed.stdout)

mod_result = fingerprint("src/foo/mod.rs", "struct Local { value: Unknown }\n")
assert mod_result["aggregate_definitions"] == [{
    "type_id": "crate::foo::Local",
    "fields": [{"name": "value"}],
    "location": {"line": 1, "column": 8},
}]
for key in ("field_accesses", "aggregate_projections", "decision_branches", "method_calls"):
    assert mod_result[key] == []

for root_path in ("src/lib.rs", "src/main.rs"):
    root_result = fingerprint(root_path, "struct Root { enabled: bool }\n")
    assert root_result["aggregate_definitions"][0]["type_id"] == "crate::Root"

imported_result = fingerprint("src/adapter.rs", '''
use crate::domain::Policy;
use crate::dto::PolicyView;

fn project(policy: &Policy) -> PolicyView {
    PolicyView { allowed: policy.allowed }
}
''')
assert imported_result["field_accesses"][0]["owner_type_id"] == "crate::domain::Policy"
assert imported_result["aggregate_projections"][0]["source_type_id"] == "crate::domain::Policy"
assert imported_result["aggregate_projections"][0]["target_type_id"] == "crate::dto::PolicyView"

generic_result = fingerprint("src/generic.rs", '''
struct Policy<T> where T: Copy {
    blocked: bool,
    state: T,
}

struct PolicyView<T>
where
    T: Copy,
{
    state: T,
}

fn project<T>(policy: &&Policy<T>) -> PolicyView<T> {
    PolicyView::<T> { state: policy.state }
}
''')
generic_definitions = {item["type_id"]: item for item in generic_result["aggregate_definitions"]}
assert set(generic_definitions) == {"crate::generic::Policy", "crate::generic::PolicyView"}
assert generic_definitions["crate::generic::Policy"]["fields"] == [
    {"name": "blocked", "type_id": "bool"},
    {"name": "state"},
]
assert generic_result["aggregate_projections"] == [{
    "source_type_id": "crate::generic::Policy",
    "target_type_id": "crate::generic::PolicyView",
    "callable_id": "crate::generic::project",
    "field_mappings": [{"source_field": "state", "target_field": "state"}],
    "location": {"line": 15, "column": 5},
}]

parent_result = fingerprint("src/domain/nested/adapter.rs", '''
struct Local { enabled: bool }

fn inspect(
    policy: &super::super::Policy,
    local: &self::Local,
    root: &crate::domain::Policy,
) -> bool {
    policy.blocked || local.enabled || root.allowed
}
''')
parent_owners = {
    (item["field"], item["owner_type_id"])
    for item in parent_result["field_accesses"]
}
assert parent_owners == {
    ("blocked", "crate::domain::Policy"),
    ("enabled", "crate::domain::nested::adapter::Local"),
    ("allowed", "crate::domain::Policy"),
}


# --- structural field type resolution -------------------------------------
#
# A field type's generic arguments are part of its identity. Stripping them to
# the constructor reports `Option<Inner>` and `Option<Other>` as the same field,
# which is how a narrowing boundary reads as a duplicated type to any consumer
# of `type_id`.

field_result = fingerprint("src/domain/shapes.rs", '''
pub struct Inner { pub a: u32 }
pub struct Other { pub b: u32 }

pub struct Wire {
    pub id: String,
    pub payload: Option<Inner>,
    pub counts: Vec<u32>,
}

pub struct Narrowed {
    pub id: String,
    pub payload: Option<Other>,
    pub counts: Vec<u32>,
}
''')
shapes = {
    definition["type_id"].split("::")[-1]: {
        field["name"]: field.get("type_id")
        for field in definition["fields"]
    }
    for definition in field_result["aggregate_definitions"]
}

# Applications resolve head-and-arguments rather than stripped to the head.
assert shapes["Wire"]["counts"] == "Vec<u32>", shapes["Wire"]
assert shapes["Wire"]["id"] == "String", shapes["Wire"]
assert shapes["Wire"]["payload"] == "Option<crate::domain::shapes::Inner>", shapes["Wire"]

# Two applications of one constructor over different arguments are different
# field types. This is the whole point: without it these two structs have an
# identical shape.
assert shapes["Wire"]["payload"] != shapes["Narrowed"]["payload"], shapes
assert shapes["Narrowed"]["payload"] == "Option<crate::domain::shapes::Other>", shapes["Narrowed"]

# Resolution is total or nothing. A partially resolved application compares
# equal to another type that differs exactly where neither side could be
# resolved, which is a false claim of sameness.
opaque_result = fingerprint("src/domain/opaque.rs", '''
pub struct Holder {
    pub handle: Option<NotDeclaredAnywhere>,
    pub known: Option<String>,
    pub nested: Vec<Option<String>>,
}
''')
opaque = {
    field["name"]: field.get("type_id")
    for definition in opaque_result["aggregate_definitions"]
    for field in definition["fields"]
}
assert opaque["handle"] is None, opaque
assert opaque["known"] == "Option<String>", opaque
assert opaque["nested"] == "Vec<Option<String>>", opaque

print("Rust policy-flow fingerprint smoke passed")
PY
