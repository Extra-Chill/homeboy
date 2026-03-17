//! markdown_rendering — extracted from codebase_map.rs.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use crate::code_audit::fingerprint::{self, FileFingerprint};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::{component, extension, Error};
use super::MapModule;
use super::MapConfig;
use super::CodebaseMap;
use super::build_class_module_index;
use super::MODULE_SPLIT_THRESHOLD;
use super::MapClass;
use super::find_source_directories_by_extension;
use super::HierarchyEntry;
use super::render_hooks_summary;
use super::derive_module_name;
use super::render_hierarchy;
use super::default_source_extensions;
use super::find_source_directories;
use super::HookSummary;
use super::split_classes_by_prefix;
use super::render_module_chunk;


/// Build a [`CodebaseMap`] from a component's source tree.
///
/// Scans source directories for files, fingerprints each one, and groups
/// results into modules by directory. Builds class hierarchy and hook summaries.
pub fn build_map(config: &MapConfig) -> Result<CodebaseMap, Error> {
    let comp = component::resolve_effective(Some(config.component_id), None, None)?;
    let root = Path::new(&comp.local_path);

    // Determine which directories to scan
    let source_dirs = if let Some(ref dirs) = config.source_dirs {
        dirs.clone()
    } else {
        let conventional = find_source_directories(root);
        if conventional.is_empty() {
            let extensions = default_source_extensions();
            find_source_directories_by_extension(root, &extensions)
        } else {
            conventional
        }
    };

    // Walk and fingerprint all source files
    let mut all_fingerprints: Vec<FileFingerprint> = Vec::new();
    for dir in &source_dirs {
        let dir_path = root.join(dir);
        if !dir_path.is_dir() {
            continue;
        }

        let scan_config = ScanConfig {
            extra_skip_dirs: vec!["tests".into(), "test".into()],
            extensions: ExtensionFilter::All,
            skip_hidden: true,
            ..Default::default()
        };
        let files = codebase_scan::walk_files(&dir_path, &scan_config);

        for file_path in files {
            if let Some(fp) = fingerprint::fingerprint_file(&file_path, root) {
                all_fingerprints.push(fp);
            }
        }
    }

    // Group fingerprints by parent directory
    let mut dir_groups: HashMap<String, Vec<&FileFingerprint>> = HashMap::new();
    for fp in &all_fingerprints {
        let parent = std::path::Path::new(&fp.relative_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        dir_groups.entry(parent).or_default().push(fp);
    }

    // Build modules from directory groups
    let mut modules: Vec<MapModule> = Vec::new();

    let mut sorted_dirs: Vec<_> = dir_groups.keys().cloned().collect();
    sorted_dirs.sort();

    for dir in &sorted_dirs {
        let fps = &dir_groups[dir];
        if fps.is_empty() {
            continue;
        }

        let mut classes: Vec<MapClass> = Vec::new();
        for fp in fps {
            let type_name = match &fp.type_name {
                Some(name) => name.clone(),
                None => continue,
            };

            let public_methods: Vec<String> = fp
                .methods
                .iter()
                .filter(|m| fp.visibility.get(*m).map(|v| v == "public").unwrap_or(true))
                .cloned()
                .collect();

            let protected_methods: Vec<String> = if config.include_private {
                fp.methods
                    .iter()
                    .filter(|m| {
                        fp.visibility
                            .get(*m)
                            .map(|v| v == "protected")
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            classes.push(MapClass {
                name: type_name,
                file: fp.relative_path.clone(),
                extends: fp.extends.clone(),
                implements: fp.implements.clone(),
                namespace: fp.namespace.clone(),
                public_methods,
                protected_methods,
                properties: fp.properties.clone(),
                hooks: fp.hooks.clone(),
            });
        }

        if classes.is_empty() {
            continue;
        }

        // Compute shared methods (methods appearing in >50% of files)
        let method_counts: HashMap<&str, usize> = {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for fp in fps {
                for method in &fp.methods {
                    if fp
                        .visibility
                        .get(method)
                        .map(|v| v == "public")
                        .unwrap_or(true)
                    {
                        *counts.entry(method.as_str()).or_default() += 1;
                    }
                }
            }
            counts
        };
        let threshold = (fps.len() as f64 * 0.5).ceil() as usize;
        let noise_methods = [
            "__construct",
            "__destruct",
            "__toString",
            "__clone",
            "__get",
            "__set",
            "__isset",
            "__unset",
            "__sleep",
            "__wakeup",
            "__invoke",
            "__debugInfo",
            "getInstance",
            "instance",
        ];
        let mut shared: Vec<String> = method_counts
            .iter()
            .filter(|(_, &count)| count >= threshold && count > 1)
            .filter(|(&name, _)| !noise_methods.contains(&name))
            .map(|(&name, _)| name.to_string())
            .collect();
        shared.sort();

        let module_name = derive_module_name(dir);

        modules.push(MapModule {
            name: module_name,
            path: dir.clone(),
            file_count: fps.len(),
            classes,
            shared_methods: shared,
        });
    }

    // Build class hierarchy (parent → children)
    let mut hierarchy_map: HashMap<String, Vec<String>> = HashMap::new();
    for fp in &all_fingerprints {
        if let (Some(ref type_name), Some(ref parent)) = (&fp.type_name, &fp.extends) {
            hierarchy_map
                .entry(parent.clone())
                .or_default()
                .push(type_name.clone());
        }
    }
    let mut class_hierarchy: Vec<HierarchyEntry> = hierarchy_map
        .into_iter()
        .map(|(parent, mut children)| {
            children.sort();
            children.dedup();
            HierarchyEntry { parent, children }
        })
        .collect();
    class_hierarchy.sort_by(|a, b| b.children.len().cmp(&a.children.len()));

    // Build hook summary
    let mut action_count = 0usize;
    let mut filter_count = 0usize;
    let mut prefix_counts: HashMap<String, usize> = HashMap::new();
    for fp in &all_fingerprints {
        for hook in &fp.hooks {
            match hook.hook_type.as_str() {
                "action" => action_count += 1,
                "filter" => filter_count += 1,
                _ => {}
            }
            let prefix = hook
                .name
                .find('_')
                .map(|i| &hook.name[..=i])
                .unwrap_or(&hook.name);
            *prefix_counts.entry(prefix.to_string()).or_default() += 1;
        }
    }
    let mut top_prefixes: Vec<(String, usize)> = prefix_counts.into_iter().collect();
    top_prefixes.sort_by(|a, b| b.1.cmp(&a.1));
    top_prefixes.truncate(10);

    let total_files = all_fingerprints.len();
    let total_classes = all_fingerprints
        .iter()
        .filter(|fp| fp.type_name.is_some())
        .count();

    Ok(CodebaseMap {
        component: config.component_id.to_string(),
        modules,
        class_hierarchy,
        hook_summary: HookSummary {
            total_actions: action_count,
            total_filters: filter_count,
            top_prefixes,
        },
        total_files,
        total_classes,
    })
}

/// Render a [`CodebaseMap`] into markdown files on disk. Returns list of created file paths.
pub fn render_map_to_markdown(map: &CodebaseMap, output_dir: &Path) -> Result<Vec<String>, Error> {
    let mut created = Vec::new();

    fs::create_dir_all(output_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create {}", output_dir.display())),
        )
    })?;

    let class_index = build_class_module_index(&map.modules);
    let children_index: HashMap<String, usize> = map
        .class_hierarchy
        .iter()
        .map(|e| (e.parent.clone(), e.children.len()))
        .collect();

    // 1. index.md
    let index = render_index(map);
    let index_path = output_dir.join("index.md");
    write_file(&index_path, &index)?;
    created.push(index_path.to_string_lossy().to_string());

    // 2. Per-module docs
    for module in &map.modules {
        let safe_name = module.path.replace('/', "-");

        if module.classes.len() > MODULE_SPLIT_THRESHOLD {
            let summary = render_module_summary(module, &safe_name);
            let summary_path = output_dir.join(format!("{}.md", safe_name));
            write_file(&summary_path, &summary)?;
            created.push(summary_path.to_string_lossy().to_string());

            let chunks = split_classes_by_prefix(&module.classes);
            for (suffix, chunk_classes) in &chunks {
                let chunk_name = format!("{}-{}", safe_name, suffix);
                let content = render_module_chunk(module, chunk_classes, suffix, &children_index);
                let chunk_path = output_dir.join(format!("{}.md", chunk_name));
                write_file(&chunk_path, &content)?;
                created.push(chunk_path.to_string_lossy().to_string());
            }
        } else {
            let filename = format!("{}.md", safe_name);
            let content = render_module(module, &children_index);
            let mod_path = output_dir.join(&filename);
            write_file(&mod_path, &content)?;
            created.push(mod_path.to_string_lossy().to_string());
        }
    }

    // 3. hierarchy.md
    let hier = render_hierarchy(&map.class_hierarchy, &class_index);
    let hier_path = output_dir.join("hierarchy.md");
    write_file(&hier_path, &hier)?;
    created.push(hier_path.to_string_lossy().to_string());

    // 4. hooks.md
    let hooks = render_hooks_summary(&map.hook_summary);
    let hooks_path = output_dir.join("hooks.md");
    write_file(&hooks_path, &hooks)?;
    created.push(hooks_path.to_string_lossy().to_string());

    Ok(created)
}

pub(crate) fn write_file(path: &Path, content: &str) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }
    fs::write(path, content)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("write {}", path.display()))))
}

pub(crate) fn render_index(map: &CodebaseMap) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", map.component));
    out.push_str(&format!(
        "{} files, {} classes, {} modules\n\n",
        map.total_files,
        map.total_classes,
        map.modules.len()
    ));
    out.push_str(&format!(
        "Hooks: {} actions, {} filters\n\n",
        map.hook_summary.total_actions, map.hook_summary.total_filters
    ));

    out.push_str("## Modules\n\n");
    out.push_str("| Module | Path | Files | Classes | Shared Methods |\n");
    out.push_str("|--------|------|------:|--------:|----------------|\n");
    for module in &map.modules {
        let shared = if module.shared_methods.is_empty() {
            "\u{2014}".to_string()
        } else {
            module.shared_methods.join(", ")
        };
        out.push_str(&format!(
            "| [{}](./{}.md) | `{}` | {} | {} | {} |\n",
            module.name,
            module.path.replace('/', "-"),
            module.path,
            module.file_count,
            module.classes.len(),
            shared
        ));
    }

    out.push_str("\n## Top Class Hierarchies\n\n");
    for entry in map.class_hierarchy.iter().take(20) {
        out.push_str(&format!(
            "- **{}** \u{2192} {} children: {}\n",
            entry.parent,
            entry.children.len(),
            entry
                .children
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out
}

pub(crate) fn render_module(module: &MapModule, children_index: &HashMap<String, usize>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} \u{2014} {}\n\n", module.name, module.path));
    out.push_str(&format!(
        "{} files, {} classes\n\n",
        module.file_count,
        module.classes.len()
    ));

    if !module.shared_methods.is_empty() {
        out.push_str(&format!(
            "**Shared interface:** {}\n\n",
            module.shared_methods.join(", ")
        ));
    }

    for class in &module.classes {
        render_class(&mut out, class, children_index);
    }

    out
}

pub(crate) fn render_module_summary(module: &MapModule, safe_name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} \u{2014} {}\n\n", module.name, module.path));
    out.push_str(&format!(
        "{} files, {} classes (split into sub-pages)\n\n",
        module.file_count,
        module.classes.len()
    ));

    if !module.shared_methods.is_empty() {
        out.push_str(&format!(
            "**Shared interface:** {}\n\n",
            module.shared_methods.join(", ")
        ));
    }

    let chunks = split_classes_by_prefix(&module.classes);
    out.push_str("## Sub-pages\n\n");
    for (suffix, chunk_classes) in &chunks {
        out.push_str(&format!(
            "- [{}](./{}-{}.md) \u{2014} {} classes\n",
            suffix,
            safe_name,
            suffix,
            chunk_classes.len()
        ));
    }

    out.push_str("\n## All Classes\n\n");
    for class in &module.classes {
        let extras = match &class.extends {
            Some(parent) => format!(" (extends {})", parent),
            None => String::new(),
        };
        out.push_str(&format!(
            "- **{}**{} \u{2014} {} public methods\n",
            class.name,
            extras,
            class.public_methods.len()
        ));
    }

    out
}

pub(crate) fn render_class(out: &mut String, class: &MapClass, children_index: &HashMap<String, usize>) {
    out.push_str(&format!("## {}\n\n", class.name));
    out.push_str(&format!("**File:** `{}`\n", class.file));

    if let Some(ref parent) = class.extends {
        out.push_str(&format!("**Extends:** {}\n", parent));
    }
    if !class.implements.is_empty() {
        out.push_str(&format!(
            "**Implements:** {}\n",
            class.implements.join(", ")
        ));
    }
    if let Some(ref ns) = class.namespace {
        out.push_str(&format!("**Namespace:** `{}`\n", ns));
    }

    if let Some(&count) = children_index.get(&class.name) {
        out.push_str(&format!(
            "**Children:** {} subclasses ([see hierarchy](./hierarchy.md))\n",
            count
        ));
    }

    out.push('\n');

    if !class.properties.is_empty() {
        out.push_str("### Properties\n\n");
        for prop in &class.properties {
            out.push_str(&format!("- `{}`\n", prop));
        }
        out.push('\n');
    }

    if !class.public_methods.is_empty() {
        let getters: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| m.starts_with("get_") || m.starts_with("get"))
            .filter(|m| !m.starts_with("get_") || m.len() > 4)
            .collect();
        let setters: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| m.starts_with("set_") || m.starts_with("set"))
            .filter(|m| !m.starts_with("set_") || m.len() > 4)
            .collect();
        let booleans: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| m.starts_with("is_") || m.starts_with("has_") || m.starts_with("can_"))
            .collect();
        let other: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| {
                !m.starts_with("get_")
                    && !m.starts_with("get")
                    && !m.starts_with("set_")
                    && !m.starts_with("set")
                    && !m.starts_with("is_")
                    && !m.starts_with("has_")
                    && !m.starts_with("can_")
            })
            .collect();

        out.push_str(&format!(
            "### Public Methods ({})\n\n",
            class.public_methods.len()
        ));

        if !getters.is_empty() {
            out.push_str(&format!(
                "**Getters ({}):** {}\n\n",
                getters.len(),
                getters
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !setters.is_empty() {
            out.push_str(&format!(
                "**Setters ({}):** {}\n\n",
                setters.len(),
                setters
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !booleans.is_empty() {
            out.push_str(&format!(
                "**Checks ({}):** {}\n\n",
                booleans.len(),
                booleans
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !other.is_empty() {
            out.push_str(&format!(
                "**Other ({}):** {}\n\n",
                other.len(),
                other
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if !class.protected_methods.is_empty() {
        out.push_str(&format!(
            "### Protected Methods ({})\n\n{}\n\n",
            class.protected_methods.len(),
            class
                .protected_methods
                .iter()
                .map(|m| format!("`{}`", m))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if !class.hooks.is_empty() {
        let actions: Vec<_> = class
            .hooks
            .iter()
            .filter(|h| h.hook_type == "action")
            .collect();
        let filters: Vec<_> = class
            .hooks
            .iter()
            .filter(|h| h.hook_type == "filter")
            .collect();

        out.push_str(&format!("### Hooks ({})\n\n", class.hooks.len()));
        if !actions.is_empty() {
            out.push_str(&format!(
                "**Actions ({}):** {}\n\n",
                actions.len(),
                actions
                    .iter()
                    .map(|h| format_hook_name(&h.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !filters.is_empty() {
            out.push_str(&format!(
                "**Filters ({}):** {}\n\n",
                filters.len(),
                filters
                    .iter()
                    .map(|h| format_hook_name(&h.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    out.push_str("---\n\n");
}

pub(crate) fn format_hook_name(name: &str) -> String {
    let is_dynamic = name.ends_with('_')
        || name.ends_with('-')
        || name.ends_with('.')
        || name.contains('{')
        || name.contains('$');
    if is_dynamic {
        format!("`{}*` *(dynamic)*", name)
    } else {
        format!("`{}`", name)
    }
}
