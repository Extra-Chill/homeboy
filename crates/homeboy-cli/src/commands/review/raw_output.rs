use crate::commands::output_runtime::CommandRun;
use homeboy_review::review::render;

use super::{run_umbrella, ReviewArgs};

pub(crate) fn run_markdown_with_json(args: ReviewArgs) -> CommandRun {
    if matches!(args.command, Some(super::ReviewCommand::Ai(_))) {
        return match super::run(args) {
            Ok((serde_json::Value::String(markdown), code)) => {
                CommandRun::from_raw_stdout("review ai", Ok(markdown), code, None)
            }
            Ok(_) => CommandRun::from_raw_stdout(
                "review ai",
                Err(homeboy::core::Error::internal_unexpected(
                    "AI review renderer did not return markdown",
                )),
                2,
                None,
            ),
            Err(error) => CommandRun::from_raw_stdout("review ai", Err(error), 2, None),
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
        Err(err) => CommandRun::from_raw_stdout("review", Err(err), 1, None),
    }
}
