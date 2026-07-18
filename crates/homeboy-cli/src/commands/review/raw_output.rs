use crate::commands::output_runtime::CommandRun;
use crate::commands::GlobalArgs;
use homeboy_review::review::render;

use super::{run_umbrella, ReviewArgs};

pub fn run_markdown_with_json(args: ReviewArgs, global: &GlobalArgs) -> CommandRun {
    let banners = args.banner.clone();
    match run_umbrella(args, global) {
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
