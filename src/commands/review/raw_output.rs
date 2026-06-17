use crate::commands::raw_output::RawCommandRun;
use crate::commands::GlobalArgs;
use homeboy::core::review::render;

use super::{run, ReviewArgs};

pub fn run_markdown_with_json(args: ReviewArgs, global: &GlobalArgs) -> RawCommandRun {
    let banners = args.banner.clone();
    match run(args, global) {
        Ok((output, exit_code)) => {
            let md = if banners.is_empty() {
                render::render_pr_comment(&output)
            } else {
                render::render_pr_comment_with_banners(&output, &banners)
            };

            RawCommandRun {
                stdout_result: Ok(md),
                exit_code,
                output_file_result: Some(serde_json::to_value(output).map_err(|err| {
                    homeboy::core::Error::internal_json(
                        err.to_string(),
                        Some("serialize response".to_string()),
                    )
                })),
            }
        }
        Err(err) => RawCommandRun {
            stdout_result: Err(err),
            exit_code: 1,
            output_file_result: None,
        },
    }
}
