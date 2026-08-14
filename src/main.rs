use homeboy::cli_runtime::CliRuntime;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("__homeboy_status_probe") {
        return homeboy::commands::status::run_status_probe_child(args.get(2));
    }
    if let Some(exit_code) = homeboy::cli_runtime::run_startup_fast_path(&args) {
        return exit_code;
    }

    CliRuntime::new().run_from_args(args)
}
