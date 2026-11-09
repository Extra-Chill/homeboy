use crate::commands::output_runtime::CommandRun;
use homeboy_review::review::render;

use super::{run_umbrella, ReviewArgs};

#[cfg(test)]
mod tests {
    #[test]
    fn failures_keep_exit_status_but_emit_markdown() {
        let run = super::markdown_failure(
            "review",
            homeboy::core::Error::internal_unexpected("preflight @stranger failed"),
            1,
        );
        assert_eq!(run.exit_code, 1);
        let markdown = run.raw_stdout.unwrap().unwrap();
        assert!(markdown.starts_with("❌"));
        assert!(!markdown.contains("@stranger"));
        assert!(!markdown.contains("homeboy/command-result"));
        assert!(run.stdout_result.is_ok());
    }
}

fn markdown_failure(command: &str, error: homeboy::core::Error, code: i32) -> CommandRun {
    CommandRun::from_raw_stdout(
        command,
        Ok(render::render_pr_comment_error(&error)),
        code,
        None,
    )
}

pub(crate) fn run_markdown_with_json(args: ReviewArgs) -> CommandRun {
    if matches!(args.command, Some(super::ReviewCommand::Ai(_))) {
        return match super::run(args) {
            Ok((serde_json::Value::String(markdown), code)) => CommandRun::from_raw_stdout(
                "review ai",
                Ok(render::safe_pr_comment(&markdown)),
                code,
                None,
            ),
            Ok(_) => markdown_failure(
                "review ai",
                homeboy::core::Error::internal_unexpected(
                    "AI review renderer did not return markdown",
                ),
                2,
            ),
            Err(error) => markdown_failure("review ai", error, 2),
        };
    }
    let banners = args.banner.clone();
    match run_umbrella(args) {
        Ok((output, exit_code)) => {
            let md = if banners.is_empty() {
                render::render_pr_comment(&output)
            } else {
                render::render_pr_comment_with_banners(&output, &banners)
            };

            CommandRun::from_raw_stdout(
                "review",
                Ok(md),
                exit_code,
                Some(serde_json::to_value(output).map_err(|err| {
                    homeboy::core::Error::internal_json(
                        err.to_string(),
                        Some("serialize response".to_string()),
                    )
                })),
            )
        }
        Err(err) => markdown_failure("review", err, 1),
    }
}
