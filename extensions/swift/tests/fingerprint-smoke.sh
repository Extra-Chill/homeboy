#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/swift"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

cat > "$TMP_DIR/input.json" <<'JSON'
{
  "file_path": "HomeboyDesktop/App/Models/AuditViewModel.swift",
  "content": "import SwiftUI\nimport Combine\n\npublic protocol AuditRunnable {\n    func runAudit()\n}\n\nfinal class AuditViewModel: ObservableObject, AuditRunnable {\n    @Published var status: String = \"idle\"\n    private let runner: AuditRunner\n\n    init(runner: AuditRunner) {\n        self.runner = runner\n    }\n\n    public func runAudit() {\n        refreshStatus()\n    }\n\n    private func refreshStatus() {\n        status = \"running\"\n    }\n}\n\nstruct AuditRunner {\n    let command: String\n}\n\nenum AuditState {\n    case idle\n    case running\n}\n\nextension AuditViewModel: Identifiable {\n    var id: String { status }\n}\n"
}
JSON

bash "$SWIFT_DIR/scripts/fingerprint.sh" < "$TMP_DIR/input.json" > "$TMP_DIR/fingerprint.json"

python3 - "$TMP_DIR/fingerprint.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

def assert_true(condition, message):
    if not condition:
        raise AssertionError(message)

symbols = data["symbols"]
assert_true("AuditViewModel" in symbols["classes"], "missing class symbol")
assert_true("AuditRunner" in symbols["structs"], "missing struct symbol")
assert_true("AuditState" in symbols["enums"], "missing enum symbol")
assert_true("AuditRunnable" in symbols["protocols"], "missing protocol symbol")
assert_true("AuditViewModel" in symbols["extensions"], "missing extension symbol")
assert_true("runAudit" in data["methods"], "missing public function")
assert_true("refreshStatus" in data["methods"], "missing private function")
assert_true("init" in data["methods"], "missing initializer")
assert_true("var status" in data["properties"], "missing property")
assert_true(data["type_name"] == "AuditRunnable", "unexpected primary type")
assert_true("ObservableObject" in data["implements"], "missing class protocol")
assert_true("Identifiable" in data["implements"], "missing extension protocol")
assert_true(data["namespace"] == "HomeboyDesktop/App/Models", "unexpected namespace")
assert_true("SwiftUI" in data["imports"], "missing import")
assert_true(data["visibility"]["runAudit"] == "public", "missing function visibility")
assert_true("runAudit" in data["public_api"], "missing public API function")
assert_true("refreshStatus" in data["internal_calls"], "missing internal call")
assert_true(data["method_hashes"].get("runAudit"), "missing method hash")

print("swift fingerprint smoke passed")
PY
