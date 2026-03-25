//! type_co_location — extracted from decompose.rs.

/// If there's only one type, everything goes in "types". If there are multiple,
/// each type gets its own group named after it (snake_case).
pub(crate) fn colocate_types(items: &[&ParsedItem]) -> Vec<(String, Vec<String>)> {
    let mut type_names: Vec<String> = Vec::new();
    let mut impl_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for item in items {
        match item.kind.as_str() {
            "struct" | "enum" | "trait" | "type_alias" => {
                type_names.push(item.name.clone());
            }
            "impl" => {
                let target = if let Some(pos) = item.name.find(" for ") {
                    item.name[pos + 5..].to_string()
                } else {
                    item.name.clone()
                };
                impl_targets
                    .entry(target)
                    .or_default()
                    .push(item.name.clone());
            }
            _ => {}
        }
    }

    if type_names.len() <= 1 {
        let mut names: Vec<String> = type_names;
        for impl_names in impl_targets.values() {
            names.extend(impl_names.iter().cloned());
        }
        if names.is_empty() {
            return Vec::new();
        }
        return vec![("types".to_string(), names)];
    }

    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut assigned_impls: HashSet<String> = HashSet::new();

    for type_name in &type_names {
        let mut group_names = vec![type_name.clone()];
        if let Some(impls) = impl_targets.get(type_name) {
            for impl_name in impls {
                group_names.push(impl_name.clone());
                assigned_impls.insert(impl_name.clone());
            }
        }
        let group_label = to_snake_case(type_name);
        groups.push((group_label, group_names));
    }

    let orphaned: Vec<String> = impl_targets
        .values()
        .flatten()
        .filter(|name| !assigned_impls.contains(*name))
        .cloned()
        .collect();

    if !orphaned.is_empty() {
        groups.push(("trait_impls".to_string(), orphaned));
    }

    groups
}

/// Convert PascalCase to snake_case.
pub(crate) fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_colocate_types_match_item_kind_as_str() {

        let result = colocate_types();
        assert!(!result.is_empty(), "expected non-empty collection for: match item.kind.as_str()");
    }

    #[test]
    fn test_colocate_types_if_let_some_impls_impl_targets_get_type_name() {

        let result = colocate_types();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(impls) = impl_targets.get(type_name) {{");
    }

    #[test]
    fn test_colocate_types_has_expected_effects() {
        // Expected effects: mutation

        let _ = colocate_types();
    }

    #[test]
    fn test_to_snake_case_default_path() {

        let _result = to_snake_case();
    }

    #[test]
    fn test_to_snake_case_has_expected_effects() {
        // Expected effects: mutation

        let _ = to_snake_case();
    }

}
