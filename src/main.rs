use homeboy::cli_runtime::CliRuntime;

fn main() -> std::process::ExitCode {
    CliRuntime::new().run_from_args(std::env::args().collect())
}
