use std::collections::HashSet;

use homeboy::core::refactor::{self, RuleResult, TransformResult};

use super::{run_across_targets, RefactorOutput, RefactorTargetArgs};
use crate::commands::CmdResult;

pub(super) fn run_transform(
    find: &str,
    replace: &str,
    files: &str,
    context: &str,
    full_match_details: bool,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("transform", targets, |component_id, path| {
        run_transform_single(
            find,
            replace,
            files,
            context,
            full_match_details,
            component_id,
            path,
            write,
        )
    })
}

fn run_transform_single(
    find: &str,
    replace: &str,
    files: &str,
    context: &str,
    full_match_details: bool,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;
    let set_name = "ad-hoc";
    let set = refactor::ad_hoc_transform(find, replace, files, context);

    homeboy::log_status!(
        "transform",
        "{} ({} rule{})",
        set_name,
        set.rules.len(),
        plural_suffix(set.rules.len())
    );

    if !set.description.is_empty() {
        homeboy::log_status!("info", "{}", set.description);
    }

    if write {
        if let Ok(preview) = refactor::apply_transforms(
            &root,
            set_name,
            &set,
            false,
            None,
            Some(refactor::DEFAULT_MATCH_DETAIL_LIMIT),
        ) {
            let affected_files: HashSet<String> = preview.modified_files.iter().cloned().collect();
            homeboy::core::engine::undo::UndoSnapshot::capture_and_save(
                &root,
                "refactor transform",
                &affected_files,
            );
        }
    }

    let result = refactor::apply_transforms(
        &root,
        set_name,
        &set,
        write,
        None,
        if full_match_details {
            None
        } else {
            Some(refactor::DEFAULT_MATCH_DETAIL_LIMIT)
        },
    )?;

    log_transform_rules(&result);
    log_transform_summary(&result, write);

    let exit_code = if result.total_replacements == 0 { 1 } else { 0 };
    Ok((RefactorOutput::Transform { result }, exit_code))
}

fn log_transform_rules(result: &TransformResult) {
    for rule_result in &result.rules {
        if rule_result.matches.is_empty() {
            homeboy::log_status!("skip", "{}: no matches", rule_result.id);
            continue;
        }

        homeboy::log_status!(
            "rule",
            "{}: {} replacement{}",
            rule_result.id,
            rule_result.replacement_count,
            plural_suffix(rule_result.replacement_count)
        );

        log_match_details(rule_result);
    }
}

fn log_match_details(rule_result: &RuleResult) {
    for m in &rule_result.matches {
        homeboy::log_status!("  match", "{}:{}", m.file, m.line);
        if !m.before.is_empty() {
            homeboy::log_status!("  -", "{}", m.before.trim());
            homeboy::log_status!("  +", "{}", m.after.trim());
        }
    }

    if rule_result.matches_truncated {
        homeboy::log_status!(
            "  omitted",
            "{} additional match detail{} omitted (use --full-match-details to include all)",
            rule_result.omitted_match_count,
            plural_suffix(rule_result.omitted_match_count)
        );
    }
}

fn log_transform_summary(result: &TransformResult, write: bool) {
    if result.total_replacements == 0 {
        homeboy::log_status!("result", "No matches found");
    } else if write {
        homeboy::log_status!(
            "result",
            "{} replacement{} applied across {} file{}",
            result.total_replacements,
            plural_suffix(result.total_replacements),
            result.total_files,
            plural_suffix(result.total_files),
        );
    } else {
        homeboy::log_status!(
            "result",
            "{} replacement{} across {} file{} (dry-run, use --write to apply)",
            result.total_replacements,
            plural_suffix(result.total_replacements),
            result.total_files,
            plural_suffix(result.total_files),
        );
    }
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}
