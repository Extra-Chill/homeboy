//! Workflow action-pin staleness.
//!
//! A workflow that pins a reusable workflow or action to a commit SHA is
//! freezing someone else's repository at a point in time. Nothing expires that
//! freeze, so a pin drifts silently and the only symptom is that fixes which
//! exist, are released, and are believed to be running are not running.
//!
//! That is not hypothetical here. `homeboy`'s CI pinned `homeboy-action` at
//! `bf6b7072`, four releases and fourteen commits behind, for long enough that
//! the differential Test gate ran a revision predating its own per-test
//! identity design — so every failed `review test` fell through to a bare
//! `current > base` count comparison and rejected changes that could not have
//! caused the delta. The `homeboy-extensions` pin beside it was 93 commits and
//! nine days stale, holding back a released fix the gate depended on. Neither
//! was detectable by any check in the repository (#13437).
//!
//! # Why this is a text scan and not a YAML parse
//!
//! Two reasons, in order of weight. A finding has to name a line for an
//! operator to act on it, and a parsed document does not carry source
//! positions. And the grammar being matched -- `uses: owner/repo@ref` and a
//! 40-hex value under `with:` -- is unambiguous in text, so a full parse buys
//! no correctness here while adding a dependency on an unmaintained crate.
//!
//! # Repository attribution
//!
//! `uses:` names its own repository. A bare SHA passed as an input does not, so
//! it is attributed to the repository of the nearest preceding `uses:` -- the
//! action that input is being passed to, which is the same rule a reader
//! applies.
//!
//! That rule is a guess, so it is **verified before it is reported**: the SHA
//! must actually exist in the attributed repository. An input that names a
//! third repository -- as `extension-ref` does, pointing at
//! `homeboy-extensions` while sitting under a `homeboy-action` `uses:` -- fails
//! that check and is reported `unresolved` rather than compared against the
//! wrong history. Declaring it in config is what makes it resolvable.
//!
//! Reporting drift against a repository that does not contain the pin would be
//! worse than reporting nothing, because it looks like an answer.
//!
//! # What "latest release" means for a multi-product repository
//!
//! The comparison target is the newest release the repository publishes,
//! whichever product line it belongs to. `homeboy-extensions` tags both
//! `wordpress-v3.x` and rust `v1.x`, so a Rust consumer can be told it is N
//! commits behind a WordPress tag. The commit count is still exactly right --
//! it is the number of commits on the default branch the pin does not include
//! -- and the label names which tag it was measured to. Only the label is
//! product-specific, and reporting the newest tag is the honest answer to
//! "what has this repository shipped since the pin".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::commands::utils::args::MutationMode;

// ---------------------------------------------------------------------------
// Discovery
//
// Pure and offline, so the attribution rules are testable without a network.
// ---------------------------------------------------------------------------

/// The shape of a pinned reference, which decides whether staleness is even a
/// meaningful question for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinForm {
    /// A 40-character hex commit SHA. Immutable, and therefore the only form
    /// that can silently fall behind.
    CommitSha,
    /// A tag or branch. It moves on its own, so it cannot go stale in the sense
    /// this check is about.
    Floating,
}

/// Where a pin was written, which decides how its repository is attributed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinSource {
    /// A `uses:` line, which names its own repository.
    Uses,
    /// A value under `with:`, which does not.
    Input { key: String },
}

/// A pin as found in the source, before any network resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredPin {
    pub file: String,
    /// 1-based, so it can be pasted into an editor or a permalink.
    pub line: usize,
    #[serde(flatten)]
    pub source: PinSource,
    /// `owner/repo`, or `None` when attribution failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// How the repository was attributed, recorded so a wrong report can be
    /// traced back to the rule that produced it.
    pub attribution: PinAttribution,
    pub reference: String,
    pub form: PinForm,
}

/// How a pin's repository was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinAttribution {
    /// Named on the `uses:` line itself. Not a guess.
    Declared,
    /// Declared in configuration for this input key. Not a guess.
    Configured,
    /// Inferred from the nearest preceding `uses:`. A guess, and verified
    /// against the remote before any drift is reported.
    NearestUses,
    /// No rule produced a repository.
    None,
}

const SHA_LEN: usize = 40;

fn is_commit_sha(value: &str) -> bool {
    value.len() == SHA_LEN && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Strip an inline `#` comment and surrounding whitespace from a scalar value.
///
/// Quoted values are returned unquoted. This is deliberately narrow: it handles
/// the forms a pin is written in, not YAML scalars in general.
fn scalar(value: &str) -> &str {
    let value = match value.find(" #") {
        Some(idx) => &value[..idx],
        None => value,
    };
    let value = value.trim();
    value
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
        .unwrap_or(value)
}

/// Split `owner/repo/some/path@ref` into `("owner/repo", "ref")`.
///
/// Returns `None` for local actions (`./path`) and for anything without both a
/// `@` and an `owner/repo` prefix, neither of which is a remote pin.
fn parse_uses(value: &str) -> Option<(String, String)> {
    if value.starts_with('.') || value.starts_with('/') {
        return None;
    }
    let (path, reference) = value.rsplit_once('@')?;
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    if owner.is_empty() || repo.is_empty() || reference.is_empty() {
        return None;
    }
    Some((format!("{owner}/{repo}"), reference.to_string()))
}

/// Extract every pin from one workflow file.
///
/// Pure and offline by construction, so the discovery rules are testable
/// without a network or a fixture repository.
///
/// `input_repositories` maps an input key to the repository it refers to, for
/// inputs whose target is not the action they are passed to.
pub fn discover_in_file(
    path: &str,
    contents: &str,
    input_repositories: &BTreeMap<String, String>,
) -> Vec<DiscoveredPin> {
    let mut pins = Vec::new();
    let mut nearest_uses_repository: Option<String> = None;

    for (index, raw) in contents.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim_start_matches("- ").trim();
        let value = scalar(value);
        if value.is_empty() {
            continue;
        }

        if key == "uses" {
            let Some((repository, reference)) = parse_uses(value) else {
                // A local action. It cannot drift, and it must not become the
                // attribution target for inputs that follow it.
                nearest_uses_repository = None;
                continue;
            };
            let form = if is_commit_sha(&reference) {
                PinForm::CommitSha
            } else {
                PinForm::Floating
            };
            nearest_uses_repository = Some(repository.clone());
            pins.push(DiscoveredPin {
                file: path.to_string(),
                line,
                source: PinSource::Uses,
                repository: Some(repository),
                attribution: PinAttribution::Declared,
                reference,
                form,
            });
            continue;
        }

        if !is_commit_sha(value) {
            continue;
        }

        let (repository, attribution) = match input_repositories.get(key) {
            Some(configured) => (Some(configured.clone()), PinAttribution::Configured),
            None => match nearest_uses_repository.clone() {
                Some(inferred) => (Some(inferred), PinAttribution::NearestUses),
                None => (None, PinAttribution::None),
            },
        };

        pins.push(DiscoveredPin {
            file: path.to_string(),
            line,
            source: PinSource::Input {
                key: key.to_string(),
            },
            repository,
            attribution,
            reference: value.to_string(),
            form: PinForm::CommitSha,
        });
    }

    pins
}

// ---------------------------------------------------------------------------
// Resolution
//
// Everything below talks to GitHub.
// ---------------------------------------------------------------------------

/// What resolution concluded about one pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinStatus {
    /// The pin is the newest released commit, or ahead of it.
    Current,
    /// Newer released commits exist that the pin does not include.
    Behind,
    /// A tag or branch. It tracks upstream on its own.
    Floating,
    /// No repository could be attributed, or the attributed repository does not
    /// contain this commit. Deliberately not `Behind`: a drift number computed
    /// against the wrong history reads like an answer.
    Unresolved,
}

/// What the remote said about a pin.
///
/// Split from [`ResolvedPin`] so each outcome is built in exactly one place.
/// Resolution has five exits, four of which are "unresolved, for this reason",
/// and as five inline struct literals they each restated three `None`s. Every
/// field is `Option`, so adding one and missing a literal compiles clean and
/// silently ships a null -- which is what happened while `target_commit` was
/// being threaded through.
#[derive(Debug, Clone, Serialize)]
pub struct PinOutcome {
    pub status: PinStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_release: Option<String>,
    /// The commit `latest_release` points at, and what a bump writes.
    ///
    /// Set only by [`PinOutcome::compared`], which is reachable only after the
    /// pin has been verified present in the attributed repository. An
    /// unresolved pin therefore cannot carry a rewrite target: the constructor
    /// makes that unrepresentable rather than leaving it to a filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_behind: Option<u64>,
    /// Why this status, in one sentence, so a failing gate is actionable
    /// without opening the API.
    pub detail: String,
}

impl PinOutcome {
    /// A tag or branch. It tracks upstream on its own.
    fn floating(reference: &str) -> Self {
        Self {
            status: PinStatus::Floating,
            latest_release: None,
            target_commit: None,
            commits_behind: None,
            detail: format!("`{reference}` tracks upstream on its own"),
        }
    }

    /// No repository could be attributed, or the attributed one does not
    /// contain the pin.
    fn unresolved(detail: String, latest_release: Option<String>) -> Self {
        Self {
            status: PinStatus::Unresolved,
            latest_release,
            target_commit: None,
            commits_behind: None,
            detail,
        }
    }

    /// The pin was found in its repository and compared against the release.
    fn compared(repository: &str, label: String, target: String, behind: u64) -> Self {
        Self {
            status: if behind == 0 {
                PinStatus::Current
            } else {
                PinStatus::Behind
            },
            detail: if behind == 0 {
                format!("at `{label}`")
            } else {
                format!("{behind} commit(s) behind `{label}` of `{repository}`")
            },
            latest_release: Some(label),
            target_commit: Some(target),
            commits_behind: Some(behind),
        }
    }
}

/// A pin plus what the remote says about it.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPin {
    #[serde(flatten)]
    pub pin: DiscoveredPin,
    #[serde(flatten)]
    pub outcome: PinOutcome,
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct PinsReport {
    pub scanned_files: usize,
    /// Tag and branch pins seen. Counted rather than listed unless `--all`,
    /// because they track upstream on their own and cannot be stale.
    pub floating: usize,
    pub pins: Vec<ResolvedPin>,
    pub behind: usize,
    pub unresolved: usize,
    /// Highest `commits_behind` across all pins, which is what a threshold is
    /// compared against.
    pub max_commits_behind: u64,
    /// Whether this run planned the rewrites or performed them.
    pub mutation_mode: MutationMode,
    /// Every rewrite a bump would make. Populated in both modes, so a plan run
    /// shows exactly what `--apply` would do.
    pub bumps: Vec<PinBump>,
    /// The subset actually written. Empty in plan mode, and a strict subset of
    /// `bumps` in apply mode when a planned line no longer matched.
    pub applied: Vec<PinBump>,
    pub remediation: Vec<String>,
}

impl PinsReport {
    /// Derive every count from the resolved pins.
    ///
    /// The counts are computed here, before any filtering, so `floating` stays
    /// truthful when `without_floating` later drops those entries from the
    /// list. Deriving them at the one place the pins arrive is what stops a
    /// summary and its own list from disagreeing.
    pub fn new(
        scanned_files: usize,
        pins: Vec<ResolvedPin>,
        bumps: Vec<PinBump>,
        applied: Vec<PinBump>,
        mutation_mode: MutationMode,
    ) -> Self {
        let count = |status: PinStatus| pins.iter().filter(|p| p.outcome.status == status).count();
        let remediation = pins
            .iter()
            .filter(|p| p.outcome.status == PinStatus::Behind)
            .map(|p| format!("{}:{} — {}", p.pin.file, p.pin.line, p.outcome.detail))
            .collect();
        Self {
            scanned_files,
            floating: count(PinStatus::Floating),
            behind: count(PinStatus::Behind),
            unresolved: count(PinStatus::Unresolved),
            max_commits_behind: pins
                .iter()
                .filter_map(|p| p.outcome.commits_behind)
                .max()
                .unwrap_or(0),
            pins,
            mutation_mode,
            bumps,
            applied,
            remediation,
        }
    }

    /// Drop tag and branch pins from the listing, keeping their count.
    ///
    /// They are the large majority of a repository's `uses:` lines and none of
    /// them can be stale, so listing them buries the finding.
    pub fn without_floating(mut self) -> Self {
        self.pins
            .retain(|pin| pin.outcome.status != PinStatus::Floating);
        self
    }

    /// Whether this report should fail its caller.
    ///
    /// Advisory unless a threshold is given. `fail_on_unresolved` is separate
    /// because an unresolved pin is a gap in this check's configuration, not
    /// evidence about the pin.
    pub fn exceeds(&self, max_commits_behind: Option<u64>, fail_on_unresolved: bool) -> bool {
        let over = max_commits_behind
            .is_some_and(|limit| self.max_commits_behind >= limit && self.max_commits_behind > 0);
        over || (fail_on_unresolved && self.unresolved > 0)
    }
}

fn gh_json(args: &[&str]) -> std::result::Result<serde_json::Value, String> {
    let output = std::process::Command::new("gh")
        .args(args)
        .output()
        .map_err(|error| format!("failed to invoke gh: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("gh returned unparseable JSON: {error}"))
}

/// The newest released tag and the commit it points at.
///
/// Falls back to the default branch head when a repository publishes no
/// releases, because "no releases" is a normal state and is not staleness.
fn latest_target(repository: &str) -> std::result::Result<(String, String), String> {
    let (label, rev) = match gh_json(&["api", &format!("repos/{repository}/releases/latest")]) {
        Ok(value) => {
            let tag = value
                .get("tag_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "latest release has no tag_name".to_string())?
                .to_string();
            (tag.clone(), tag)
        }
        Err(_) => {
            let value = gh_json(&["api", &format!("repos/{repository}")])?;
            let branch = value
                .get("default_branch")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "repository has no default_branch".to_string())?
                .to_string();
            (format!("{branch} (no releases published)"), branch)
        }
    };
    let commit = gh_json(&["api", &format!("repos/{repository}/commits/{rev}")])?
        .get("sha")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("could not resolve `{rev}` to a commit"))?
        .to_string();
    Ok((label, commit))
}

/// Resolve one discovered pin against its remote.
///
/// Thin by design: every exit builds a [`PinOutcome`] through a constructor,
/// and the pin is joined to its outcome exactly once, here.
pub fn resolve_pin(pin: DiscoveredPin) -> ResolvedPin {
    let outcome = outcome_for(&pin);
    ResolvedPin { pin, outcome }
}

fn outcome_for(pin: &DiscoveredPin) -> PinOutcome {
    if pin.form == PinForm::Floating {
        return PinOutcome::floating(&pin.reference);
    }

    let Some(repository) = pin.repository.as_deref() else {
        let key = match &pin.source {
            PinSource::Input { key } => key.as_str(),
            PinSource::Uses => "uses",
        };
        return PinOutcome::unresolved(
            format!(
                "no repository could be attributed to `{key}`; declare it under `ci.pin_repositories` to make this pin checkable"
            ),
            None,
        );
    };

    let (label, target) = match latest_target(repository) {
        Ok(found) => found,
        Err(error) => {
            return PinOutcome::unresolved(
                format!("could not read the latest release of `{repository}`: {error}"),
                None,
            )
        }
    };

    // This is also the attribution check. A pin inferred from the nearest
    // `uses:` may name a repository it does not belong to, and comparing it
    // there would invent a drift number. The compare call fails for exactly
    // that case, so a wrong guess degrades to `unresolved` rather than lying.
    let compare = gh_json(&[
        "api",
        &format!("repos/{repository}/compare/{}...{target}", pin.reference),
    ]);
    let ahead = match compare {
        Ok(value) => value.get("ahead_by").and_then(|v| v.as_u64()),
        Err(error) => {
            let note = if pin.attribution == PinAttribution::NearestUses {
                format!(
                    " -- `{repository}` was inferred from the nearest `uses:`, so this pin most likely belongs to a different repository; declare it under `ci.pin_repositories`"
                )
            } else {
                String::new()
            };
            return PinOutcome::unresolved(
                format!(
                    "`{}` is not reachable in `{repository}`: {error}{note}",
                    pin.reference
                ),
                Some(label),
            );
        }
    };

    match ahead {
        Some(behind) => PinOutcome::compared(repository, label, target, behind),
        None => PinOutcome::unresolved(
            format!("`{repository}` comparison returned no `ahead_by`"),
            Some(label),
        ),
    }
}

// ---------------------------------------------------------------------------
// Bumping
//
// Rewrites a behind pin to the commit resolution proved reachable.
// ---------------------------------------------------------------------------

/// One pin rewrite: what would change, or what did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PinBump {
    pub file: String,
    pub line: usize,
    pub repository: String,
    pub from: String,
    pub to: String,
    /// The release the new commit belongs to, for the commit message.
    pub release: String,
}

/// Which pins a bump would touch.
///
/// Only `Behind` pins qualify. `Unresolved` is excluded by construction rather
/// than by a filter: it carries no `target_commit`, because a target is only
/// recorded after the pin has been verified reachable in the repository it was
/// attributed to. A bump therefore cannot act on an attribution guess, which is
/// the one way this command could do real damage.
pub fn plan_bumps(pins: &[ResolvedPin]) -> Vec<PinBump> {
    pins.iter()
        .filter(|pin| pin.outcome.status == PinStatus::Behind)
        .filter_map(|pin| {
            Some(PinBump {
                file: pin.pin.file.clone(),
                line: pin.pin.line,
                repository: pin.pin.repository.clone()?,
                from: pin.pin.reference.clone(),
                to: pin.outcome.target_commit.clone()?,
                release: pin.outcome.latest_release.clone().unwrap_or_default(),
            })
        })
        .collect()
}

/// Apply bumps to one file's text.
///
/// Replacement is anchored to `(line, from)` rather than done globally: the
/// same SHA legitimately appears on several lines -- `uses:` and `action-ref:`
/// carry the identical commit -- and each is a distinct pin that must be
/// rewritten on its own terms. A global replace would also rewrite the SHA
/// where it appears in a prose comment, which is not a pin and may be a
/// deliberate historical reference.
///
/// A bump whose line no longer contains `from` is skipped and reported, rather
/// than applied to whatever moved into that position.
pub fn apply_bumps_to_text(contents: &str, bumps: &[PinBump]) -> (String, Vec<PinBump>) {
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let mut applied = Vec::new();
    for bump in bumps {
        let Some(line) = lines.get_mut(bump.line.saturating_sub(1)) else {
            continue;
        };
        if !line.contains(&bump.from) {
            continue;
        }
        *line = line.replace(&bump.from, &bump.to);
        applied.push(bump.clone());
    }
    let mut out = lines.join("\n");
    // `str::lines` drops the trailing newline; YAML files carry one and losing
    // it would put a spurious hunk in every bump diff.
    if contents.ends_with('\n') {
        out.push('\n');
    }
    (out, applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mapping() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn mapping(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The wire vocabulary these statuses serialize to is what a consumer
    /// filters on, so it is pinned against literals rather than against a
    /// second spelling of itself. An `as_str` here would only have restated
    /// `#[serde(rename_all)]` with nothing tying the two together -- the defect
    /// in #13400, invented rather than inherited.
    #[test]
    fn the_wire_vocabulary_is_pinned() {
        let form = |f| serde_json::to_value(f).expect("serialize");
        assert_eq!(form(PinForm::CommitSha), "commit_sha");
        assert_eq!(form(PinForm::Floating), "floating");

        let status = |s| serde_json::to_value(s).expect("serialize");
        assert_eq!(status(PinStatus::Current), "current");
        assert_eq!(status(PinStatus::Behind), "behind");
        assert_eq!(status(PinStatus::Floating), "floating");
        assert_eq!(status(PinStatus::Unresolved), "unresolved");
    }

    #[test]
    fn a_uses_line_names_its_own_repository() {
        let pins = discover_in_file(
            "ci.yml",
            "    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@83110d6638ec1c2f63352fcb004920524add0377\n",
            &no_mapping(),
        );
        assert_eq!(pins.len(), 1);
        assert_eq!(
            pins[0].repository.as_deref(),
            Some("Extra-Chill/homeboy-action")
        );
        assert_eq!(pins[0].attribution, PinAttribution::Declared);
        assert_eq!(pins[0].form, PinForm::CommitSha);
        assert_eq!(pins[0].line, 1);
    }

    /// A tag or branch moves on its own, so it is reported but is not a
    /// staleness candidate. Classifying it as a stale commit pin would be a
    /// permanent false positive.
    #[test]
    fn a_tag_is_floating_rather_than_stale() {
        let pins = discover_in_file("ci.yml", "  uses: actions/checkout@v6\n", &no_mapping());
        assert_eq!(pins[0].form, PinForm::Floating);
        assert_eq!(pins[0].reference, "v6");
    }

    /// The rule that would have caught the `action-ref` half of #13437: a bare
    /// SHA under `with:` belongs to the action it is passed to.
    #[test]
    fn a_bare_sha_input_is_attributed_to_the_nearest_uses() {
        let yaml = "\
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@83110d6638ec1c2f63352fcb004920524add0377
    with:
      action-ref: 83110d6638ec1c2f63352fcb004920524add0377
";
        let pins = discover_in_file("ci.yml", yaml, &no_mapping());
        assert_eq!(pins.len(), 2);
        assert_eq!(
            pins[1].source,
            PinSource::Input {
                key: "action-ref".to_string()
            }
        );
        assert_eq!(
            pins[1].repository.as_deref(),
            Some("Extra-Chill/homeboy-action")
        );
        assert_eq!(pins[1].attribution, PinAttribution::NearestUses);
    }

    /// The `extension-ref` case. It sits under a `homeboy-action` `uses:` but
    /// refers to `homeboy-extensions`, so nearest-uses attribution is wrong
    /// here and configuration has to win. Getting this backwards would compare
    /// the pin against a history that does not contain it.
    #[test]
    fn configuration_overrides_nearest_uses_for_an_input_naming_a_third_repository() {
        let yaml = "\
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@83110d6638ec1c2f63352fcb004920524add0377
    with:
      extension-ref: fd4165ef2b9713672d0e25786c3af27c03012aae
";
        let pins = discover_in_file(
            "ci.yml",
            yaml,
            &mapping(&[("extension-ref", "Extra-Chill/homeboy-extensions")]),
        );
        assert_eq!(
            pins[1].repository.as_deref(),
            Some("Extra-Chill/homeboy-extensions")
        );
        assert_eq!(pins[1].attribution, PinAttribution::Configured);
    }

    /// A local action cannot drift, and -- the part that is easy to get wrong
    /// -- it must not become the attribution target for inputs beneath it.
    #[test]
    fn a_local_action_is_not_a_pin_and_does_not_attribute_inputs() {
        let yaml = "\
      - uses: ./.homeboy-action
        with:
          action-ref: 83110d6638ec1c2f63352fcb004920524add0377
";
        let pins = discover_in_file("ci.yml", yaml, &no_mapping());
        assert_eq!(pins.len(), 1, "the local `uses:` is not itself a pin");
        assert_eq!(pins[0].repository, None);
        assert_eq!(pins[0].attribution, PinAttribution::None);
    }

    #[test]
    fn comments_and_inline_comments_are_not_pins() {
        let yaml = "\
    # uses: Extra-Chill/homeboy-action@bf6b7072e6cb11a21d14c0f78ccd33beade24f48
    uses: actions/checkout@v6 # pinned deliberately
";
        let pins = discover_in_file("ci.yml", yaml, &no_mapping());
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].reference, "v6");
    }

    /// Quoted scalars are the same pin as unquoted ones.
    #[test]
    fn quoted_values_are_unwrapped() {
        let yaml = "      action-ref: '83110d6638ec1c2f63352fcb004920524add0377'\n";
        let pins = discover_in_file("ci.yml", yaml, &no_mapping());
        assert_eq!(pins.len(), 1);
        assert_eq!(
            pins[0].reference,
            "83110d6638ec1c2f63352fcb004920524add0377"
        );
    }

    /// Not every 40-character value is a SHA, and a non-hex string must not be
    /// reported as a pin against some repository's history.
    #[test]
    fn a_forty_character_non_hex_value_is_not_a_pin() {
        let yaml = "      some-key: zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n";
        assert!(discover_in_file("ci.yml", yaml, &no_mapping()).is_empty());
    }

    fn resolved(
        file: &str,
        line: usize,
        repository: Option<&str>,
        reference: &str,
        status: PinStatus,
        target: Option<&str>,
    ) -> ResolvedPin {
        ResolvedPin {
            pin: DiscoveredPin {
                file: file.to_string(),
                line,
                source: PinSource::Uses,
                repository: repository.map(str::to_string),
                attribution: PinAttribution::Declared,
                reference: reference.to_string(),
                form: PinForm::CommitSha,
            },
            outcome: PinOutcome {
                status,
                latest_release: Some("v2.15.9".to_string()),
                target_commit: target.map(str::to_string),
                commits_behind: Some(6),
                detail: String::new(),
            },
        }
    }

    /// The safety property. An `unresolved` pin is one whose repository could
    /// not be verified, so rewriting it would be acting on a guess. It carries
    /// no `target_commit`, so it cannot be planned even if the filter were
    /// wrong -- both guards are asserted here.
    #[test]
    fn only_behind_pins_are_bumped_and_unresolved_ones_carry_no_target() {
        let pins = vec![
            resolved(
                "ci.yml",
                1,
                Some("o/r"),
                "a".repeat(40).as_str(),
                PinStatus::Behind,
                Some(&"b".repeat(40)),
            ),
            resolved(
                "ci.yml",
                2,
                Some("o/r"),
                "c".repeat(40).as_str(),
                PinStatus::Current,
                Some(&"c".repeat(40)),
            ),
            resolved(
                "ci.yml",
                3,
                Some("o/r"),
                "d".repeat(40).as_str(),
                PinStatus::Unresolved,
                None,
            ),
        ];
        let bumps = plan_bumps(&pins);
        assert_eq!(bumps.len(), 1, "only the behind pin is bumped");
        assert_eq!(bumps[0].line, 1);
    }

    /// The second, independent guard -- and the one that actually holds.
    ///
    /// Asserting `target_commit.is_none()` on a fixture the test itself built
    /// with `None` proves nothing about the code; it restates the fixture. The
    /// guarantee lives in the constructor, so the constructor is what is
    /// called here. A rewrite target can only be produced by `compared`, which
    /// resolution reaches only after verifying the pin exists in the
    /// repository it was attributed to.
    #[test]
    fn an_unresolved_outcome_cannot_carry_a_rewrite_target() {
        let outcome = PinOutcome::unresolved("not reachable".to_string(), Some("v1".to_string()));
        assert_eq!(outcome.status, PinStatus::Unresolved);
        assert!(outcome.target_commit.is_none());
        assert!(outcome.commits_behind.is_none());

        let compared = PinOutcome::compared("o/r", "v1".to_string(), "c".repeat(40), 3);
        assert_eq!(compared.status, PinStatus::Behind);
        assert_eq!(
            compared.target_commit.as_deref(),
            Some("c".repeat(40).as_str())
        );
    }

    /// `compared` with zero drift is `current`, not `behind`, and still carries
    /// a target -- so a re-run after a bump does not propose the same bump.
    #[test]
    fn a_pin_at_its_release_is_current() {
        let outcome = PinOutcome::compared("o/r", "v1".to_string(), "c".repeat(40), 0);
        assert_eq!(outcome.status, PinStatus::Current);
        assert_eq!(outcome.commits_behind, Some(0));
        assert!(plan_bumps(&[]).is_empty());
    }

    /// `uses:` and `action-ref:` carry the identical SHA on different lines.
    /// Each is a separate pin, and a global replace would conflate them with
    /// any other occurrence -- including one inside a prose comment.
    #[test]
    fn the_same_sha_on_several_lines_is_rewritten_per_line() {
        let old = "b".repeat(40);
        let new = "c".repeat(40);
        let yaml = format!(
            "    uses: o/r@{old}\n    with:\n      action-ref: {old}\n    # historical: {old}\n"
        );
        let bumps = vec![
            PinBump {
                file: "ci.yml".into(),
                line: 1,
                repository: "o/r".into(),
                from: old.clone(),
                to: new.clone(),
                release: "v1".into(),
            },
            PinBump {
                file: "ci.yml".into(),
                line: 3,
                repository: "o/r".into(),
                from: old.clone(),
                to: new.clone(),
                release: "v1".into(),
            },
        ];
        let (out, applied) = apply_bumps_to_text(&yaml, &bumps);
        assert_eq!(applied.len(), 2);
        assert_eq!(out.matches(&new).count(), 2, "both pins rewritten");
        assert!(
            out.contains(&format!("# historical: {old}")),
            "the comment occurrence is untouched"
        );
    }

    /// A stale plan must not be applied to whatever moved into that line.
    #[test]
    fn a_bump_whose_line_no_longer_matches_is_skipped() {
        let bumps = vec![PinBump {
            file: "ci.yml".into(),
            line: 1,
            repository: "o/r".into(),
            from: "b".repeat(40),
            to: "c".repeat(40),
            release: "v1".into(),
        }];
        let (out, applied) = apply_bumps_to_text("    uses: o/r@v6\n", &bumps);
        assert!(applied.is_empty());
        assert_eq!(out, "    uses: o/r@v6\n");
    }

    /// Losing the trailing newline would put a spurious hunk in every diff.
    #[test]
    fn the_trailing_newline_survives_a_bump() {
        let old = "b".repeat(40);
        let bumps = vec![PinBump {
            file: "ci.yml".into(),
            line: 1,
            repository: "o/r".into(),
            from: old.clone(),
            to: "c".repeat(40),
            release: "v1".into(),
        }];
        let (out, _) = apply_bumps_to_text(&format!("    uses: o/r@{old}\n"), &bumps);
        assert!(out.ends_with('\n'));
    }

    /// The real shape of the file this check exists for: three pins, two
    /// repositories, one of which only configuration can attribute.
    #[test]
    fn the_regression_shape_from_13437_is_fully_discovered() {
        let yaml = "\
jobs:
  homeboy:
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@bf6b7072e6cb11a21d14c0f78ccd33beade24f48
    with:
      component: homeboy
      action-ref: bf6b7072e6cb11a21d14c0f78ccd33beade24f48
      extension-ref: 691d0527d7a4b3917649043a7f91a81913a139ab
";
        let pins = discover_in_file(
            "ci.yml",
            yaml,
            &mapping(&[("extension-ref", "Extra-Chill/homeboy-extensions")]),
        );
        assert_eq!(pins.len(), 3);
        let described: Vec<_> = pins
            .iter()
            .map(|pin| {
                (
                    pin.repository.as_deref().unwrap_or("<unresolved>"),
                    pin.attribution,
                    pin.line,
                )
            })
            .collect();
        assert_eq!(
            described,
            vec![
                ("Extra-Chill/homeboy-action", PinAttribution::Declared, 3),
                ("Extra-Chill/homeboy-action", PinAttribution::NearestUses, 6),
                (
                    "Extra-Chill/homeboy-extensions",
                    PinAttribution::Configured,
                    7
                ),
            ]
        );
    }
}

// ---------------------------------------------------------------------------
// Filesystem
//
// The only part that touches disk. Kept beside the pin logic rather than in the
// command handler, so `run_pins` reads as orchestration.
// ---------------------------------------------------------------------------

/// Read every workflow file under `dir`, labelled relative to `root`.
///
/// Sorted, so the report is diffable run to run. Unreadable entries are skipped
/// rather than failing the command: one bad file should not hide the pins in
/// every other one.
pub fn read_workflow_files(root: &Path, dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("yml") | Some("yaml")
            )
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let contents = std::fs::read_to_string(&path).ok()?;
            let label = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            Some((label, contents))
        })
        .collect()
}

/// Apply bumps to the files they name, returning only those actually written.
///
/// Grouped by file so each is read and written once. A file that cannot be read
/// or written contributes nothing, which keeps `applied` an honest record of
/// what is on disk rather than what was intended.
pub fn apply_bumps_to_files(root: &Path, bumps: &[PinBump]) -> Vec<PinBump> {
    let mut by_file: BTreeMap<&str, Vec<PinBump>> = BTreeMap::new();
    for bump in bumps {
        by_file
            .entry(bump.file.as_str())
            .or_default()
            .push(bump.clone());
    }

    let mut applied = Vec::new();
    for (file, file_bumps) in by_file {
        let path = root.join(file);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (rewritten, done) = apply_bumps_to_text(&contents, &file_bumps);
        if done.is_empty() {
            continue;
        }
        if std::fs::write(&path, rewritten).is_ok() {
            applied.extend(done);
        }
    }
    applied
}
