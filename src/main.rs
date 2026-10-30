use homeboy::cli_runtime::CliRuntime;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // Stable, store-free installer query. Never initialize or migrate state here.
    if args.get(1).map(String::as_str) == Some("--observation-schema-version") {
        println!(
            "{}",
            homeboy_core::observation::store::CURRENT_SCHEMA_VERSION
        );
        return std::process::ExitCode::SUCCESS;
    }
    if args.get(1).map(String::as_str) == Some("__homeboy_status_probe") {
        return homeboy::commands::status::run_status_probe_child(args.get(2));
    }
    if let Some(exit_code) = homeboy::cli_runtime::run_update_help_fast_path(&args) {
        return exit_code;
    }
    let runtime = CliRuntime::new();
    if let Some(exit_code) = runtime.run_startup_fast_path(&args) {
        return exit_code;
    }
    runtime.run_from_args(args)
}
