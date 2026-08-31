//! Canonical extension ownership and execution-context resolution.

use crate::component::Component;
use crate::error::{Error, ErrorCode, Result};
use crate::project::Project;
use std::path::{Path, PathBuf};

use crate::extension::catalog::{extension_path, load_extension, load_extension_in_root};
use crate::extension::invoke::ResolvedExtensionInvocationContext;
use homeboy_extension_contract::{ExtensionCapability, ExtensionManifest};

mod catalog_resolution;
mod requirements;
mod surface;

use catalog_resolution::CapabilityCatalog;

pub use requirements::{validate_extension_requirements, validate_required_extensions};
pub use surface::{
    find_installed_file_extension, find_installed_file_extension_in_root, resolve_file_extension,
    resolve_file_extension_in_root, FileExtensionCapability, FILE_EXTENSIONS_SURFACE,
    REMOTE_PATH_SURFACE, SINCE_TAG_SURFACE,
};

/// Whether an extension's declared dependencies are available in this context.
pub fn is_extension_compatible(extension: &ExtensionManifest, project: Option<&Project>) -> bool {
    let Some(requires) = extension.requires.as_ref() else {
        return true;
    };

    if requires
        .extensions
        .iter()
        .any(|extension_id| load_extension(extension_id).is_err())
    {
        return false;
    }

    project.is_none_or(|project| {
        requires
            .components
            .iter()
            .all(|component_id| crate::project::has_component(project, component_id))
    })
}

pub fn stderr_tail(stderr: &str) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = stderr.lines().collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

#[derive(Debug, Clone)]
pub struct ExtensionExecutionContext {
    pub component: Component,
    pub capability: ExtensionCapability,
    pub extension_id: String,
    pub extension_path: PathBuf,
    pub script_path: String,
    pub settings: Vec<(String, serde_json::Value)>,
    /// Setting keys the resolved extension declares it understands (from
    /// the manifest `settings` block). Used to validate `--setting` /
    /// `--setting-json` overrides before a run. Empty means the extension
    /// declares no settings, in which case validation is skipped.
    pub accepted_setting_keys: Vec<String>,
}

pub fn path_list_env_value(error_field: &str, paths: &[PathBuf]) -> Result<String> {
    let path_description = error_field
        .strip_suffix("_workloads")
        .map(|prefix| format!("{prefix} workload path"))
        .unwrap_or_else(|| "workload path".to_string());

    std::env::join_paths(paths)
        .map_err(|e| {
            Error::validation_invalid_argument(
                error_field,
                format!("{path_description} cannot be exported: {e}"),
                None,
                None,
            )
        })
        .map(|joined| joined.to_string_lossy().to_string())
}

fn no_extensions_error(component: &Component) -> Error {
    let mut err = Error::new(
        ErrorCode::ExtensionUnsupported,
        format!(
            "No extension provider configured for component '{}'",
            component.id
        ),
        serde_json::json!({
            "component_id": component.id,
            "problem": "no extensions configured",
        }),
    );

    for hint in extension_guidance_hints(component, None) {
        err = err.with_hint(hint);
    }

    err
}

fn capability_missing_error(component: &Component, capability: ExtensionCapability) -> Error {
    let capability_name = capability.label();
    let mut err = Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has no linked extensions that provide {} support",
            component.id, capability_name
        ),
        None,
        None,
    );

    for hint in extension_guidance_hints(component, Some(capability)) {
        err = err.with_hint(hint);
    }

    err
}

pub fn extension_guidance_hints(
    component: &Component,
    capability: Option<ExtensionCapability>,
) -> Vec<String> {
    let link_hint = match capability {
        Some(capability) => format!(
            "Link an extension with {} support: homeboy component set {} --extension <extension_id>",
            capability.label(),
            component.id
        ),
        None => format!(
            "Link an extension that provides the needed command support: homeboy component set {} --extension <extension_id>",
            component.id
        ),
    };

    // Point at the component-owned escape hatch too: a component can supply its
    // own command via a `scripts.<capability>` entry without linking an
    // extension at all. Named after the capability so the hint is actionable
    // (e.g. `scripts.build`) and independently regression-covered.
    let scripts_hint = match capability {
        Some(capability) => format!(
            "Use `scripts.{}` for component-owned {} commands (no extension required): set it in the component config.",
            capability.label().to_lowercase(),
            capability.label().to_lowercase()
        ),
        None => "Use `scripts.<command>` for component-owned commands (no extension required): set it in the component config."
            .to_string(),
    };

    vec![
        link_hint,
        scripts_hint,
        "List installed extensions: homeboy extension list".to_string(),
        format!(
            "Component config lives at ~/.config/homeboy/components/{}.json or in a portable homeboy.json discovered from the component path.",
            component.id
        ),
        "The component path resolved correctly; the requested command needs an extension provider.".to_string(),
    ]
}

/// Ambiguity error for a contested ownership surface.
///
/// The hint must be a *runnable command*, not advice. Before #11120 this said
/// "Configure explicit {capability} extension ownership before running this
/// command", which named neither the config key (`capability_extensions`) nor
/// any way to set it — while the validation errors a few dozen lines below
/// correctly reported `capability_extensions.{label}`. A caller who hit the
/// ambiguity had no discoverable route forward, because `--capability-extension`
/// did not exist either. Both halves are fixed together: the flag exists now, so
/// the hint names it with the surface and a concrete candidate filled in.
fn ownership_ambiguous_error(component: &Component, surface: &str, matching: &[String]) -> Error {
    let first = matching
        .first()
        .map(String::as_str)
        .unwrap_or("<extension_id>");

    Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has multiple linked extensions providing '{}': {}",
            component.id,
            surface,
            matching.join(", ")
        ),
        None,
        Some(
            matching
                .iter()
                .map(|candidate| {
                    format!(
                        "homeboy component set {} --capability-extension {}={}",
                        component.id, surface, candidate
                    )
                })
                .collect(),
        ),
    )
    .with_hint(format!(
        "Pick the owning extension: homeboy component set {} --capability-extension {}={}",
        component.id, surface, first
    ))
}

fn explicit_surface_extension<'a>(component: &'a Component, surface: &str) -> Option<&'a str> {
    component
        .capability_extensions
        .get(surface)
        .map(String::as_str)
        .map(str::trim)
        .filter(|extension_id| !extension_id.is_empty())
}

fn explicit_capability_extension(
    component: &Component,
    capability: ExtensionCapability,
) -> Option<&str> {
    explicit_surface_extension(component, capability.label())
}

pub fn extract_component_extension_settings(
    component: &Component,
    extension_id: &str,
) -> Vec<(String, serde_json::Value)> {
    component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(extension_id))
        .map(|extension_config| {
            extension_config
                .settings
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_extension_for_capability(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<String> {
    match resolve_extension_for_capability_if_available(component, capability)? {
        Some(extension_id) => Ok(extension_id),
        None if component
            .extensions
            .as_ref()
            .is_none_or(|extensions| extensions.is_empty()) =>
        {
            Err(no_extensions_error(component))
        }
        None => Err(capability_missing_error(component, capability)),
    }
}

/// Resolve a capability only when one of the component's linked extensions
/// advertises it. This lets optional consumers skip a capability that the
/// component has not opted into without weakening validation of explicit or
/// ambiguous capability ownership.
fn resolve_extension_for_capability_if_available(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<Option<String>> {
    let Some(extensions) = component.extensions.as_ref() else {
        return Ok(None);
    };
    if extensions.is_empty() {
        return Ok(None);
    }
    let catalog = CapabilityCatalog::load()?;

    if let Some(extension_id) = explicit_capability_extension(component, capability) {
        if !extensions.contains_key(extension_id) {
            return Err(Error::validation_invalid_argument(
                format!("capability_extensions.{}", capability.label()),
                format!(
                    "Component '{}' selects extension '{}' for {} support, but it is not linked",
                    component.id,
                    extension_id,
                    capability.label()
                ),
                Some(extension_id.to_string()),
                Some(vec![format!(
                    "Add '{}' to extensions or choose one of: {}",
                    extension_id,
                    extensions.keys().cloned().collect::<Vec<_>>().join(", ")
                )]),
            ));
        }

        let entry = catalog.resolvable_entry(extension_id)?;
        if !catalog.provides(entry, capability) {
            return Err(Error::validation_invalid_argument(
                format!("capability_extensions.{}", capability.label()),
                format!(
                    "Component '{}' selects extension '{}' for {} support, but that extension does not provide it",
                    component.id,
                    extension_id,
                    capability.label()
                ),
                Some(extension_id.to_string()),
                None,
            ));
        }

        return Ok(Some(extension_id.to_string()));
    }

    let (mut matching, catalog_failures) = catalog.candidates(extensions.keys(), capability);

    match matching.len() {
        // Nothing advertises the capability. Invalid catalog entries keep the
        // answer unknown rather than silently reporting "not provided".
        0 if catalog_failures.is_empty() => Ok(None),
        0 => Err(unreadable_catalog_error(
            component,
            capability,
            &catalog_failures,
        )),
        1 => Ok(Some(matching.remove(0))),
        _ => disambiguate_capability_owner(component, capability, &matching).map(Some),
    }
}

/// Reported only when a capability could not be satisfied *and* at least one
/// linked catalog entry was invalid or missing.
fn unreadable_catalog_error(
    component: &Component,
    capability: ExtensionCapability,
    failures: &[(String, String)],
) -> Error {
    let names = failures
        .iter()
        .map(|failure| failure.0.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let mut err = Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has no linked extensions that provide {} support, and {} \
             manifest(s) could not be read: {}",
            component.id,
            capability.label(),
            failures.len(),
            names
        ),
        None,
        Some(
            failures
                .iter()
                .map(|failure| format!("Extension '{}' failed to load: {}", failure.0, failure.1))
                .collect(),
        ),
    );

    err = err.with_hint(format!(
        "Repair or reinstall the named extension(s): homeboy extension install {}",
        failures
            .first()
            .map(|failure| failure.0.as_str())
            .unwrap_or("<extension_id>")
    ));

    for hint in extension_guidance_hints(component, Some(capability)) {
        err = err.with_hint(hint);
    }

    err
}

/// Pick the owning extension when several linked extensions provide the same
/// capability.
///
/// This is the single ownership rule for multi-extension components, shared by
/// capability execution resolution and by artifact-pattern resolution
/// ([`crate::component::resolve_artifact`]) so both stay deterministic and
/// agree on who owns a contested capability:
///
/// 1. explicit `capability_extensions.<capability>` selection, when it names
///    one of the candidates;
/// 2. `composition.includes` primacy — the manifests can encode which linked
///    extension is primary: a component that links WordPress + Node.js has
///    WordPress declare `includes: ["nodejs"]`, so WordPress owns the shared
///    capabilities and Node.js is the composed subordinate. When exactly one
///    of the candidates includes all the others, resolve to it instead of
///    forcing a manual `capability_extensions` selection;
/// 3. otherwise the ambiguity is genuine and the component author must resolve
///    it, so this returns [`capability_ambiguous_error`].
///
/// `candidates` must be in a deterministic order (the error message lists
/// them), and must never be empty.
pub(crate) fn disambiguate_capability_owner(
    component: &Component,
    capability: ExtensionCapability,
    candidates: &[String],
) -> Result<String> {
    resolve_owner(component, capability.label(), candidates)
}

/// [`disambiguate_capability_owner`] against an already-resolved config root
/// (#7505).
pub(crate) fn disambiguate_capability_owner_in_root(
    config_root: &Path,
    component: &Component,
    capability: ExtensionCapability,
    candidates: &[String],
) -> Result<String> {
    resolve_owner_in_root(config_root, component, capability.label(), candidates)
}

/// Pick the owning extension for an arbitrary contested *surface*.
///
/// This is [`disambiguate_capability_owner`] with the seven-variant
/// [`ExtensionCapability`] enum lifted off the front, so surfaces that have no
/// enum variant — `remote_path`, `since_tag`, `provides.file_extensions` — can
/// share the one ownership rule instead of inventing a seventh contradictory
/// one (#11119). `Component.capability_extensions` is already
/// `HashMap<String, String>` with arbitrary string keys, so an entry like
/// `capability_extensions."remote_path"` needs no schema change.
///
/// The rule, unchanged:
///
/// 1. explicit `capability_extensions.<surface>` selection, when it names one
///    of the candidates;
/// 2. `composition.includes` primacy — when exactly one candidate composes all
///    the others (WordPress declares `includes: ["nodejs"]`), it owns the
///    surface;
/// 3. otherwise the ambiguity is genuine → [`ownership_ambiguous_error`], which
///    carries a runnable `homeboy component set --capability-extension` command.
///
/// `candidates` must be deterministically ordered — callers build them from a
/// `BTreeMap`/`BTreeSet`, never from `Component.extensions` (a `HashMap` with
/// `RandomState`, whose iteration order differs every process).
pub fn resolve_owner(
    component: &Component,
    surface: &str,
    candidates: &[String],
) -> Result<String> {
    if let Some(explicit) = explicit_surface_extension(component, surface) {
        if let Some(selected) = candidates.iter().find(|id| id.as_str() == explicit) {
            return Ok(selected.clone());
        }
    }

    if let Some(primary) = composition_primary_extension(candidates)? {
        return Ok(primary);
    }

    Err(ownership_ambiguous_error(component, surface, candidates))
}

/// [`resolve_owner`] against an already-resolved config root (#7505).
///
/// Only the composition-primacy rung reads manifests, so only that rung is
/// rooted; the explicit `capability_extensions` rung and the ambiguity error are
/// pure functions of the component and candidate list. A caller that resolves
/// components from an injected home reaches this so the primacy answer comes
/// from the same home the component did, instead of whichever extensions happen
/// to be installed in the ambient one.
pub fn resolve_owner_in_root(
    config_root: &Path,
    component: &Component,
    surface: &str,
    candidates: &[String],
) -> Result<String> {
    if let Some(explicit) = explicit_surface_extension(component, surface) {
        if let Some(selected) = candidates.iter().find(|id| id.as_str() == explicit) {
            return Ok(selected.clone());
        }
    }

    if let Some(primary) = composition_primary_extension_in_root(config_root, candidates)? {
        return Ok(primary);
    }

    Err(ownership_ambiguous_error(component, surface, candidates))
}

/// If exactly one of the ambiguous extensions composes all of the others via its
/// `composition.includes`, return it as the primary owner. Returns `None` when
/// no single extension includes every other (leaving the ambiguity unresolved).
fn composition_primary_extension(matching: &[String]) -> Result<Option<String>> {
    let declarations = matching
        .iter()
        // A candidate that will not load cannot claim primacy, but it must not
        // take the other candidates' claims down with it either (#11122).
        .filter_map(|candidate| {
            let manifest = load_extension(candidate).ok()?;
            let composition = manifest.composition.as_ref()?;
            Some((candidate.clone(), composition.includes.clone()))
        })
        .collect::<Vec<_>>();
    Ok(composition_primary_from_declarations(
        matching,
        &declarations,
    ))
}

/// [`composition_primary_extension`] against an already-resolved config root.
fn composition_primary_extension_in_root(
    config_root: &Path,
    matching: &[String],
) -> Result<Option<String>> {
    let declarations = matching
        .iter()
        .filter_map(|candidate| {
            let manifest = load_extension_in_root(config_root, candidate).ok()?;
            let composition = manifest.composition.as_ref()?;
            Some((candidate.clone(), composition.includes.clone()))
        })
        .collect::<Vec<_>>();
    Ok(composition_primary_from_declarations(
        matching,
        &declarations,
    ))
}

/// The primacy rule itself, over already-read `composition.includes` lists.
///
/// Kept separate from manifest loading so the ambient and rooted resolvers share
/// one rule and can only differ in which home they read manifests from.
fn composition_primary_from_declarations(
    matching: &[String],
    declarations: &[(String, Vec<String>)],
) -> Option<String> {
    let mut primary: Option<String> = None;
    for (candidate, includes) in declarations {
        let includes_all_others = matching
            .iter()
            .filter(|other| *other != candidate)
            .all(|other| includes.iter().any(|inc| inc == other));
        if includes_all_others {
            if primary.is_some() {
                // More than one extension claims to include the others; the
                // composition is not unambiguous, so do not guess.
                return None;
            }
            primary = Some(candidate.clone());
        }
    }
    primary
}

/// Whether a linked extension can provide a capability without requiring one.
///
/// Callers that can safely skip an optional capability use this to distinguish
/// an absent provider from invalid explicit capability ownership.
pub fn has_linked_extension_for_capability(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<bool> {
    let Some(extensions) = component.extensions.as_ref() else {
        return Ok(false);
    };
    if extensions.is_empty() {
        return Ok(false);
    }
    if explicit_capability_extension(component, capability).is_some() {
        resolve_extension_for_capability(component, capability)?;
        return Ok(true);
    }

    // An unreadable sibling manifest does not get to answer this question for
    // the extensions that *are* readable (#11122). A probe that finds a
    // provider is `true` regardless; only a probe that finds none has to decide
    // what an unreadable manifest means, and "no provider" is the honest answer
    // for an optional capability — the caller that goes on to actually run it
    // gets the named load failure from `resolve_extension_for_capability`.
    let catalog = CapabilityCatalog::load()?;
    let (matching, _) = catalog.candidates(extensions.keys(), capability);
    Ok(!matching.is_empty())
}

pub fn resolve_execution_context(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<ExtensionExecutionContext> {
    let extension_id = resolve_extension_for_capability(component, capability)?;
    execution_context_for_extension(component, None, capability, extension_id)
}

/// Resolve an execution context when a linked extension provides `capability`.
/// A missing optional capability is represented as `Ok(None)`; malformed
/// explicit ownership and ambiguous providers remain validation errors.
pub fn resolve_execution_context_if_available(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<Option<ExtensionExecutionContext>> {
    let Some(extension_id) = resolve_extension_for_capability_if_available(component, capability)?
    else {
        return Ok(None);
    };
    execution_context_for_extension(component, None, capability, extension_id).map(Some)
}

/// Resolve an execution context for one named linked extension.
///
/// Owner-elected capabilities (Lint, Test, Bench, Trace, Deps, Build, Fuzz)
/// resolve exactly one provider and should use [`resolve_execution_context`].
/// Aggregating capabilities compose a result from every linked extension that
/// advertises them — audit reference paths are the union of each extension's
/// contribution, not one extension's answer — so they need a context bound to a
/// specific extension rather than an elected owner.
///
/// The extension must be linked to the component and must advertise
/// `capability`; both are validation errors rather than silent skips.
pub fn resolve_execution_context_for_extension(
    component: &Component,
    capability: ExtensionCapability,
    extension_id: &str,
) -> Result<ExtensionExecutionContext> {
    let linked = component
        .extensions
        .as_ref()
        .is_some_and(|extensions| extensions.contains_key(extension_id));
    if !linked {
        return Err(Error::validation_invalid_argument(
            "extension",
            format!(
                "Component '{}' does not link extension '{}'",
                component.id, extension_id
            ),
            Some(extension_id.to_string()),
            None,
        ));
    }

    let catalog = CapabilityCatalog::load()?;
    let entry = catalog.resolvable_entry(extension_id)?;
    if !catalog.provides(entry, capability) {
        return Err(Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' does not provide {} support",
                extension_id,
                capability.label()
            ),
            Some(extension_id.to_string()),
            None,
        ));
    }

    execution_context_for_extension(component, None, capability, extension_id.to_string())
}

/// Resolve a capability against a project attachment, retaining the project
/// settings and attachment path for the runner environment.
pub fn resolve_execution_context_for_project(
    project: &crate::project::Project,
    component_id: &str,
    capability: ExtensionCapability,
) -> Result<ExtensionExecutionContext> {
    let component = crate::project::resolve_project_component(project, component_id)?;
    let extension_id = resolve_extension_for_capability(&component, capability)?;
    execution_context_for_extension(&component, Some(project.clone()), capability, extension_id)
}

fn execution_context_for_extension(
    component: &Component,
    project: Option<crate::project::Project>,
    capability: ExtensionCapability,
    extension_id: String,
) -> Result<ExtensionExecutionContext> {
    let manifest = load_extension(&extension_id)?;
    let script_path = capability
        .script_path(&manifest)
        .map(|s| s.to_string())
        // Build's extension_script is optional (builds can use local scripts or command templates),
        // so we allow an empty script_path for Build. Lint/Test/Bench require it.
        .or_else(|| {
            if capability.requires_script() {
                None
            } else {
                Some(String::new())
            }
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension '{}' does not have {} infrastructure configured",
                    extension_id,
                    capability.label()
                ),
                None,
                None,
            )
        })?;

    let extension_path = extension_path(&extension_id);

    if !extension_path.exists() {
        return Err(Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' not found in ~/.config/homeboy/extensions/",
                extension_id
            ),
            None,
            None,
        ));
    }

    let invocation = ResolvedExtensionInvocationContext::for_component(
        &extension_id,
        project,
        component.clone(),
    )?;

    Ok(ExtensionExecutionContext {
        component: invocation
            .component
            .expect("capability invocation has component"),
        capability,
        extension_id: extension_id.clone(),
        extension_path,
        script_path,
        settings: invocation.settings.into_iter().collect(),
        accepted_setting_keys: manifest.accepted_setting_keys(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ScopedExtensionConfig;

    fn write_extension_manifest(home: &Path, extension_id: &str, capability: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            format!(
                r#"{{"name":"{extension_id}","version":"1.0.0","{capability}":{{"extension_script":"{capability}.sh"}}}}"#
            ),
        )
        .expect("extension manifest");
    }

    fn component_with_extensions(extension_ids: &[&str]) -> Component {
        let extensions = extension_ids
            .iter()
            .map(|extension_id| {
                (
                    (*extension_id).to_string(),
                    ScopedExtensionConfig::default(),
                )
            })
            .collect();

        Component {
            id: "consumer".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        }
    }

    fn write_extension_manifest_with_includes(
        home: &Path,
        extension_id: &str,
        capability: &str,
        includes: &[&str],
    ) {
        let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let includes_json = includes
            .iter()
            .map(|inc| format!("\"{inc}\""))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            format!(
                r#"{{"name":"{extension_id}","version":"1.0.0","{capability}":{{"extension_script":"{capability}.sh"}},"composition":{{"includes":[{includes_json}]}}}}"#
            ),
        )
        .expect("extension manifest");
    }

    fn write_file_extension_manifest(home: &Path, extension_id: &str, includes: &[&str]) {
        let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let includes_json = includes
            .iter()
            .map(|included| format!("\"{included}\""))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            format!(
                r#"{{"name":"{extension_id}","version":"1.0.0","provides":{{"file_extensions":["js"],"capabilities":["fingerprint"]}},"scripts":{{"fingerprint":"fingerprint.sh"}},"composition":{{"includes":[{includes_json}]}}}}"#
            ),
        )
        .expect("extension manifest");
    }

    #[test]
    fn component_file_resolution_considers_only_linked_extensions() {
        crate::test_support::with_isolated_home(|home| {
            write_file_extension_manifest(home.path(), "alpha", &[]);
            write_file_extension_manifest(home.path(), "zulu", &[]);

            let component = component_with_extensions(&["zulu"]);
            let resolved =
                resolve_file_extension(&component, "js", FileExtensionCapability::Fingerprint)
                    .expect("linked provider resolution")
                    .expect("linked provider");

            assert_eq!(resolved.id, "zulu");
        });
    }

    #[test]
    fn component_file_resolution_uses_composition_and_explicit_ownership() {
        crate::test_support::with_isolated_home(|home| {
            write_file_extension_manifest(home.path(), "wordpress", &["nodejs"]);
            write_file_extension_manifest(home.path(), "nodejs", &[]);

            let mut component = component_with_extensions(&["wordpress", "nodejs"]);
            let composed =
                resolve_file_extension(&component, "js", FileExtensionCapability::Fingerprint)
                    .expect("composition resolution")
                    .expect("composition owner");
            assert_eq!(composed.id, "wordpress");

            component
                .capability_extensions
                .insert(FILE_EXTENSIONS_SURFACE.to_string(), "nodejs".to_string());
            let explicit =
                resolve_file_extension(&component, "js", FileExtensionCapability::Fingerprint)
                    .expect("explicit resolution")
                    .expect("explicit owner");
            assert_eq!(explicit.id, "nodejs");
        });
    }

    #[test]
    fn component_file_resolution_rejects_genuine_ambiguity() {
        crate::test_support::with_isolated_home(|home| {
            write_file_extension_manifest(home.path(), "alpha", &[]);
            write_file_extension_manifest(home.path(), "zulu", &[]);

            let component = component_with_extensions(&["zulu", "alpha"]);
            let error =
                resolve_file_extension(&component, "js", FileExtensionCapability::Fingerprint)
                    .expect_err("unowned providers must remain ambiguous");

            assert!(error
                .message
                .contains("multiple linked extensions providing 'provides.file_extensions'"));
        });
    }

    #[test]
    fn installed_file_resolution_retains_deterministic_fallback() {
        crate::test_support::with_isolated_home(|home| {
            write_file_extension_manifest(home.path(), "zulu", &[]);
            write_file_extension_manifest(home.path(), "alpha", &[]);

            let resolved =
                find_installed_file_extension("js", FileExtensionCapability::Fingerprint)
                    .expect("installed fallback");

            assert_eq!(resolved.id, "alpha");
        });
    }

    #[test]
    fn compatibility_requires_installed_extensions_and_project_components() {
        crate::test_support::with_isolated_home(|home| {
            let extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "name": "consumer",
                "version": "1.0.0",
                "requires": {
                    "extensions": ["provider"],
                    "components": ["database"]
                }
            }))
            .expect("extension manifest");
            let project = Project {
                id: "site".to_string(),
                components: vec![crate::project::ProjectComponentAttachment {
                    id: "database".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            assert!(!is_extension_compatible(&extension, Some(&project)));

            write_extension_manifest(home.path(), "provider", "deps");
            assert!(is_extension_compatible(&extension, Some(&project)));

            let missing_component = Project {
                id: "other".to_string(),
                ..Default::default()
            };
            assert!(!is_extension_compatible(
                &extension,
                Some(&missing_component)
            ));
        });
    }

    #[test]
    fn rooted_file_resolution_stays_within_the_injected_config_root() {
        crate::test_support::with_isolated_home(|home| {
            let injected_home = home.path().join("injected");
            let config_root = injected_home.join(".config/homeboy");
            write_file_extension_manifest(&injected_home, "zulu", &[]);

            let component = component_with_extensions(&["zulu"]);
            let resolved = resolve_file_extension_in_root(
                &config_root,
                &component,
                "js",
                FileExtensionCapability::Fingerprint,
            )
            .expect("rooted provider resolution")
            .expect("rooted provider");
            assert_eq!(resolved.id, "zulu");

            let installed = find_installed_file_extension_in_root(
                &config_root,
                "js",
                FileExtensionCapability::Fingerprint,
            )
            .expect("rooted installed fallback");
            assert_eq!(installed.id, "zulu");
            assert!(
                find_installed_file_extension("js", FileExtensionCapability::Fingerprint).is_none()
            );
        });
    }

    #[test]
    fn composition_includes_resolves_ambiguous_capability_to_primary() {
        crate::test_support::with_isolated_home(|home| {
            // WordPress and Node.js both provide `deps`; WordPress composes
            // Node.js via `composition.includes`, so it is the primary owner.
            write_extension_manifest_with_includes(home.path(), "wordpress", "deps", &["nodejs"]);
            write_extension_manifest(home.path(), "nodejs", "deps");

            let component = component_with_extensions(&["wordpress", "nodejs"]);
            assert_eq!(
                resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                    .expect("composition.includes should resolve the ambiguity"),
                "wordpress"
            );
        });
    }

    #[test]
    fn explicit_capability_extension_overrides_composition_primary() {
        crate::test_support::with_isolated_home(|home| {
            write_extension_manifest_with_includes(home.path(), "wordpress", "deps", &["nodejs"]);
            write_extension_manifest(home.path(), "nodejs", "deps");

            let mut component = component_with_extensions(&["wordpress", "nodejs"]);
            // Explicit selection must still win over the composition primary.
            component
                .capability_extensions
                .insert("deps".to_string(), "nodejs".to_string());
            assert_eq!(
                resolve_extension_for_capability(&component, ExtensionCapability::Deps).unwrap(),
                "nodejs"
            );
        });
    }

    #[test]
    fn ambiguous_capability_without_composition_still_errors() {
        crate::test_support::with_isolated_home(|home| {
            // Neither includes the other -> ambiguity is preserved.
            write_extension_manifest(home.path(), "wordpress", "deps");
            write_extension_manifest(home.path(), "nodejs", "deps");

            let component = component_with_extensions(&["wordpress", "nodejs"]);
            let err = resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                .expect_err("no composition primary should remain ambiguous");
            assert!(err
                .message
                .contains("multiple linked extensions providing 'deps'"));
        });
    }

    #[test]
    fn explicit_capability_extension_resolves_ambiguous_deps_owner() {
        crate::test_support::with_isolated_home(|home| {
            write_extension_manifest(home.path(), "wordpress", "deps");
            write_extension_manifest(home.path(), "nodejs", "deps");

            let mut component = component_with_extensions(&["wordpress", "nodejs"]);
            let err = resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                .expect_err("multiple deps providers should be ambiguous without ownership");
            assert!(err
                .message
                .contains("multiple linked extensions providing 'deps'"));

            component
                .capability_extensions
                .insert("deps".to_string(), "nodejs".to_string());

            assert_eq!(
                resolve_extension_for_capability(&component, ExtensionCapability::Deps).unwrap(),
                "nodejs"
            );
        });
    }

    fn write_unreadable_extension_manifest(home: &Path, extension_id: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            // A nested manifest struct carrying a key no current release
            // accepts. `StructuredSidecarDetail` is `deny_unknown_fields` and
            // sits behind an `untagged` enum, so one retired key here fails the
            // entire manifest — which is exactly what retiring a key does to an
            // already-published manifest, and why `ProvidesConfig` had the
            // attribute removed.
            format!(
                r#"{{"name":"{extension_id}","version":"1.0.0","deps":{{"extension_script":"deps.sh"}},"structured_sidecars":{{"lint.findings":{{"enabled":true,"retired_key_from_an_older_release":true}}}}}}"#
            ),
        )
        .expect("extension manifest");

        // Pin the fixture's premise where it is cheap to read: if this manifest
        // ever becomes loadable, every test below would pass vacuously.
        assert!(
            load_extension(extension_id).is_err(),
            "fixture must be an unreadable manifest"
        );
    }

    /// One malformed manifest used to black out every linked extension: the
    /// resolution loop did `load_extension(id)?`, so a broken `wordpress` made
    /// `nodejs`'s capability unresolvable even though nodejs was intact.
    /// (#11122)
    #[test]
    fn broken_sibling_manifest_does_not_black_out_a_working_extension() {
        crate::test_support::with_isolated_home(|home| {
            write_unreadable_extension_manifest(home.path(), "wordpress");
            write_extension_manifest(home.path(), "nodejs", "deps");

            let component = component_with_extensions(&["wordpress", "nodejs"]);

            assert_eq!(
                resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                    .expect("an intact extension must still resolve"),
                "nodejs"
            );
            assert!(
                has_linked_extension_for_capability(&component, ExtensionCapability::Deps)
                    .expect("probe must not fail on a broken sibling")
            );
        });
    }

    /// The failure is surfaced — with the offending extension named — only when
    /// the capability cannot be satisfied without it.
    #[test]
    fn unreadable_manifest_is_reported_when_it_is_the_only_candidate() {
        crate::test_support::with_isolated_home(|home| {
            write_unreadable_extension_manifest(home.path(), "wordpress");

            let component = component_with_extensions(&["wordpress"]);
            let err = resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                .expect_err("an unreadable sole provider is not a silent absence");

            assert!(
                err.message.contains("wordpress"),
                "the failing extension must be named: {}",
                err.message
            );
            assert!(
                err.message.contains("could not be read"),
                "a load failure must not be reported as 'no provider configured': {}",
                err.message
            );
        });
    }

    /// A broken manifest that is irrelevant to the requested capability stays
    /// irrelevant: this is still an honest "no provider", not a load error.
    #[test]
    fn unreadable_manifest_does_not_mask_a_genuine_capability_miss() {
        crate::test_support::with_isolated_home(|home| {
            write_extension_manifest(home.path(), "nodejs", "deps");

            let component = component_with_extensions(&["nodejs"]);
            let err = resolve_extension_for_capability(&component, ExtensionCapability::Bench)
                .expect_err("nodejs provides deps, not bench");

            assert!(
                err.message.contains("no linked extensions that provide"),
                "{}",
                err.message
            );
            assert!(
                !err.message.contains("could not be read"),
                "{}",
                err.message
            );
        });
    }

    /// `Component.extensions` is a `HashMap`, so an unsorted survey would list
    /// candidates in a different order every process and make both the
    /// ambiguity error and the load-failure error nondeterministic.
    #[test]
    fn catalog_resolution_orders_candidates_and_failures_deterministically() {
        crate::test_support::with_isolated_home(|home| {
            for extension_id in ["zulu", "alpha", "mike"] {
                write_extension_manifest(home.path(), extension_id, "deps");
            }
            for extension_id in ["yankee", "bravo"] {
                write_unreadable_extension_manifest(home.path(), extension_id);
            }

            let ids: Vec<String> = ["zulu", "alpha", "mike", "yankee", "bravo"]
                .iter()
                .map(|id| (*id).to_string())
                .collect();
            let catalog = CapabilityCatalog::load().expect("v1 catalog");
            let (matching, failures) = catalog.candidates(ids.iter(), ExtensionCapability::Deps);

            assert_eq!(matching, vec!["alpha", "mike", "zulu"]);
            assert_eq!(
                failures
                    .iter()
                    .map(|failure| failure.0.as_str())
                    .collect::<Vec<_>>(),
                vec!["bravo", "yankee"]
            );
        });
    }

    #[test]
    fn explicit_capability_extension_must_be_linked_and_supported() {
        crate::test_support::with_isolated_home(|home| {
            write_extension_manifest(home.path(), "nodejs", "deps");
            write_extension_manifest(home.path(), "wordpress", "bench");

            let mut component = component_with_extensions(&["nodejs"]);
            component
                .capability_extensions
                .insert("deps".to_string(), "wordpress".to_string());
            let err = resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                .expect_err("selected extension must be linked");
            assert!(err.message.contains("but it is not linked"));

            component = component_with_extensions(&["wordpress"]);
            component
                .capability_extensions
                .insert("deps".to_string(), "wordpress".to_string());
            let err = resolve_extension_for_capability(&component, ExtensionCapability::Deps)
                .expect_err("selected extension must provide selected capability");
            assert!(err.message.contains("does not provide it"));
        });
    }
}
