#!/usr/bin/env python3
"""Canonical Rust test identities shared by inventory and result adapters."""


def canonical_test_id(package, target_kind, target, name):
    values = (package, target_kind, target, name)
    if not all(isinstance(value, str) and value for value in values):
        raise ValueError("Rust test identity fields must be non-empty strings")
    return "::".join(values)


def inventory_test(package, target_kind, target, name, expected_outcome="executed"):
    return {
        "id": canonical_test_id(package, target_kind, target, name),
        "package": package,
        "target": target,
        "target_kind": target_kind,
        "name": name,
        "expected_outcome": expected_outcome,
    }
