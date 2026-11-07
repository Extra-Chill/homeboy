//! Private `__homeboy_runtime_json` child protocol.
//!
//! The core-shipped extension runtime shell helpers invoke the running binary
//! (`HOMEBOY_RUNTIME_EXECUTABLE`) with this argv[1] to perform their JSON work.
//! It is dispatched from `main` before CLI runtime initialization, like the
//! status probe child, because helpers call it once per emitted record.
//!
//! Operations:
//!   sidecar-write-array <target> [item...]
//!   sidecar-write-object <target> <item>
//!   sidecar-append <target> <item>
//!   sidecar-merge <target> <source>
//!   test-failure [--test-id ID --suite S --file F --line N --message M
//!                 --failure-type T --identity I --stdout-excerpt O --stderr-excerpt E]
//!   test-failure-excerpt            (raw output on stdin)
//!   bench-artifact-ref <path> [kind] [label]
//!   bench-now-ms
//!   bench-responsiveness-ping <file> <started-ms> [label]
//!   bench-results [--inventory] [--results-file F] [--component ID] [--iterations N]
//!                 [--metadata-json J] [--metric-groups-json J] [--span-definitions-json J]
//!                 [--artifacts-json J] [--timeline-json J] [--extras-file F] [--] [record...]

use homeboy_core::extension::runtime_json::{bench, sidecar, test_failure};
use std::io::{Read, Write};
use std::process::ExitCode;

#[derive(Debug)]
enum Failure {
    /// Invalid invocation; exit 2 with the message as-is.
    Usage(String),
    /// Operation error; exit 1 with the message as-is.
    Runtime(String),
}

/// Entry point used only by the runtime helper child protocol. `args` excludes
/// the program name and the protocol selector.
pub fn run_runtime_json_child(args: &[String]) -> ExitCode {
    match run(args) {
        Ok(stdout) => {
            let mut out = std::io::stdout().lock();
            if out
                .write_all(stdout.as_bytes())
                .and_then(|()| out.flush())
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(Failure::Usage(message)) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, Failure> {
    let (operation, rest) = args
        .split_first()
        .ok_or_else(|| Failure::Usage("[runtime-json] operation is required".to_string()))?;
    let arg = |index: usize| rest.get(index).map(String::as_str).unwrap_or_default();
    let sidecar_error = |error: String| Failure::Runtime(format!("[sidecar-writer] {error}"));
    match operation.as_str() {
        "sidecar-write-array" => sidecar::write_array(arg(0), rest.get(1..).unwrap_or_default())
            .map_err(sidecar_error)?,
        "sidecar-write-object" => sidecar::write_object(arg(0), arg(1)).map_err(sidecar_error)?,
        "sidecar-append" => sidecar::append(arg(0), arg(1)).map_err(sidecar_error)?,
        "sidecar-merge" => sidecar::merge(arg(0), arg(1)).map_err(sidecar_error)?,
        "test-failure" => {
            let record = test_failure::record(&parse_test_failure(rest)?);
            return Ok(format!("{}\n", record.to_compact()));
        }
        "test-failure-excerpt" => {
            let mut raw = Vec::new();
            std::io::stdin()
                .read_to_end(&mut raw)
                .map_err(|error| Failure::Runtime(format!("[emit-test-failure] {error}")))?;
            return Ok(test_failure::bound_excerpt(&String::from_utf8_lossy(&raw)));
        }
        "bench-artifact-ref" => {
            if arg(0).is_empty() {
                return Err(Failure::Usage(
                    "homeboy_bench_artifact_ref_json: path is required".to_string(),
                ));
            }
            let reference = bench::artifact_ref(arg(0), arg(1), arg(2));
            return Ok(format!("{}\n", reference.to_compact_ascii()));
        }
        "bench-now-ms" => return Ok(format!("{}\n", bench::now_ms())),
        "bench-responsiveness-ping" => {
            let started_ms = arg(1).trim().parse::<i64>().map_err(|_| {
                Failure::Usage(format!(
                    "homeboy_bench_responsiveness_ping: invalid start time `{}`",
                    arg(1)
                ))
            })?;
            bench::responsiveness_ping(arg(0), started_ms, arg(2)).map_err(|error| {
                Failure::Runtime(format!("homeboy_bench_responsiveness_ping: {error}"))
            })?;
        }
        "bench-results" => {
            const PREFIX: &str = "homeboy_write_bench_results_from_payload_files";
            let request = parse_bench_results(rest)?;
            let warnings = bench::write_results(&request)
                .map_err(|error| Failure::Runtime(format!("{PREFIX}: {error}")))?;
            for warning in warnings {
                eprintln!("{warning}");
            }
        }
        other => {
            return Err(Failure::Usage(format!(
                "[runtime-json] unknown operation: {other}"
            )))
        }
    }
    Ok(String::new())
}

fn parse_test_failure(args: &[String]) -> Result<test_failure::TestFailureInput, Failure> {
    let mut input = test_failure::TestFailureInput::default();
    let mut index = 0;
    while index < args.len() {
        let value = args.get(index + 1).cloned().unwrap_or_default();
        match args[index].as_str() {
            "--test-id" => input.test_id = value,
            "--suite" => input.suite = value,
            "--file" => input.file = value,
            "--line" => input.line = value,
            "--message" => input.message = value,
            "--failure-type" => input.failure_type = value,
            "--identity" => input.identity = Some(value),
            "--stdout-excerpt" => input.stdout_excerpt = value,
            "--stderr-excerpt" => input.stderr_excerpt = value,
            other => {
                return Err(Failure::Usage(format!(
                    "[emit-test-failure] unknown argument: {other}"
                )))
            }
        }
        index += 2;
    }
    if input.test_id.is_empty() {
        return Err(Failure::Usage(
            "[emit-test-failure] --test-id is required".to_string(),
        ));
    }
    Ok(input)
}

/// Flags mirror the shell function, which passes its runner-variable defaults
/// as leading flags; a later repeat of a flag wins.
fn parse_bench_results(args: &[String]) -> Result<bench::BenchResultsRequest, Failure> {
    const PREFIX: &str = "homeboy_write_bench_results_from_payload_files";
    let mut request = bench::BenchResultsRequest::default();
    let mut iterations = String::new();
    let mut index = 0;
    while index < args.len() {
        let value = args.get(index + 1).cloned().unwrap_or_default();
        let slot = match args[index].as_str() {
            "--inventory" => {
                request.inventory = true;
                index += 1;
                continue;
            }
            "--" => {
                request.records.extend(args[index + 1..].iter().cloned());
                break;
            }
            "--results-file" => &mut request.results_file,
            "--component" | "--component-id" => &mut request.component_id,
            "--iterations" => &mut iterations,
            "--metadata-json" => &mut request.metadata_json,
            "--metric-groups-json" => &mut request.metric_groups_json,
            "--span-definitions-json" => &mut request.span_definitions_json,
            "--artifacts-json" => &mut request.artifacts_json,
            "--timeline-json" => &mut request.timeline_json,
            "--extras-file" => &mut request.extras_file,
            record => {
                request.records.push(record.to_string());
                index += 1;
                continue;
            }
        };
        *slot = value;
        index += 2;
    }
    if request.component_id.is_empty() {
        return Err(Failure::Usage(format!(
            "{PREFIX}: component id is required"
        )));
    }
    if request.results_file.is_empty() {
        return Err(Failure::Usage(format!(
            "{PREFIX}: HOMEBOY_BENCH_RESULTS_FILE is required"
        )));
    }
    request.iterations = if iterations.trim().is_empty() {
        0
    } else {
        iterations
            .trim()
            .parse()
            .map_err(|_| Failure::Usage(format!("{PREFIX}: invalid iterations `{iterations}`")))?
    };
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn test_failure_requires_test_id_and_rejects_unknown_flags() {
        assert!(matches!(
            parse_test_failure(&args(&["--message", "boom"])),
            Err(Failure::Usage(message)) if message.contains("--test-id is required")
        ));
        assert!(matches!(
            parse_test_failure(&args(&["--test-id", "a", "--bogus", "x"])),
            Err(Failure::Usage(message)) if message.contains("unknown argument: --bogus")
        ));
        let input = parse_test_failure(&args(&["--test-id", "a", "--identity", ""])).unwrap();
        assert_eq!(input.identity.as_deref(), Some(""));
    }

    #[test]
    fn bench_results_flags_records_and_separator() {
        let request = parse_bench_results(&args(&[
            "--results-file",
            "/tmp/r.json",
            "--component",
            "demo",
            "--iterations",
            "4",
            "--metadata-json",
            "{\"a\":1}",
            "first=p.json",
            "--",
            "--looks-like-flag=x",
        ]))
        .unwrap();
        assert_eq!(request.results_file, "/tmp/r.json");
        assert_eq!(request.component_id, "demo");
        assert_eq!(request.iterations, 4);
        assert_eq!(request.metadata_json, "{\"a\":1}");
        assert_eq!(
            request.records,
            args(&["first=p.json", "--looks-like-flag=x"])
        );
    }
}
