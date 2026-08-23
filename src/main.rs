use homeboy::cli_runtime::CliRuntime;

static PRODUCT_CAPABILITIES: [&'static dyn homeboy::cli_runtime::CliCapability; 1] =
    [&homeboy_triage::TRIAGE_CAPABILITY];

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("__homeboy_status_probe") {
        return homeboy::commands::status::run_status_probe_child(args.get(2));
    }
    let runtime = CliRuntime::with_capabilities(&PRODUCT_CAPABILITIES);
    runtime.run_from_args(args)
}
