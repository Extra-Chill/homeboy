use super::*;

#[test]
fn filter_homeboy_flags_strips_bench_flags_and_preserves_runner_args() {
    let cases: &[(&[&str], &[&str])] = &[
        (&[], &[]),
        (&["--ratchet", "--filter=Scenario"], &["--filter=Scenario"]),
        (
            &[
                "--baseline",
                "--ignore-baseline",
                "--ratchet",
                "--json-summary",
            ],
            &[],
        ),
        (
            &["--iterations", "50", "--filter=Scenario"],
            &["--filter=Scenario"],
        ),
        (&["--iterations=50", "--keep"], &["--keep"]),
        (
            &["--warmup", "3", "--filter=Scenario"],
            &["--filter=Scenario"],
        ),
        (&["--warmup=3", "--keep"], &["--keep"]),
        (
            &[
                "--shared-state",
                "/tmp/foo",
                "--concurrency",
                "4",
                "--filter=Scenario",
            ],
            &["--filter=Scenario"],
        ),
        (
            &["--shared-state=/tmp/foo", "--concurrency=4", "--keep"],
            &["--keep"],
        ),
        (
            &["--rig-order", "reverse", "--filter=Scenario"],
            &["--filter=Scenario"],
        ),
        (&["--rig-order=reverse", "--keep"], &["--keep"]),
        (
            &["--report", "side-by-side", "--filter=Scenario"],
            &["--filter=Scenario"],
        ),
        (&["--report=side-by-side", "--keep"], &["--keep"]),
        (
            &["--rig-concurrency", "2", "--filter=Scenario"],
            &["--filter=Scenario"],
        ),
        (&["--rig-concurrency=2", "--keep"], &["--keep"]),
        (&["--regression-threshold", "10", "--keep"], &["--keep"]),
        (&["--regression-threshold=10", "--keep"], &["--keep"]),
        (
            &["--filter=Scenario", "--verbose", "extra"],
            &["--filter=Scenario", "--verbose", "extra"],
        ),
        (
            &[
                "--ratchet",
                "--iterations",
                "25",
                "--filter=hot_path",
                "--regression-threshold=7.5",
                "--verbose",
            ],
            &["--filter=hot_path", "--verbose"],
        ),
    ];

    for (input, expected) in cases {
        let args: Vec<String> = input.iter().map(|arg| arg.to_string()).collect();
        assert_eq!(filter_homeboy_flags(&args), *expected, "input: {input:?}");
    }
}

#[test]
fn parses_placement_after_bench_without_losing_settings() {
    let normalized = crate::commands::utils::args::normalize(
        [
            "homeboy",
            "bench",
            "--rig",
            "studio-bfb",
            "--iterations",
            "1",
            "--placement",
            "local",
            "--setting",
            "studio_site_build_prompt_variant=astro-docs-content-collection",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    );

    let cli = crate::cli_surface::Cli::try_parse_from(normalized)
        .expect("bench --placement local --setting should parse");

    assert_eq!(cli.placement, crate::cli_surface::Placement::Local);
    let crate::cli_surface::Commands::Bench(args) = cli.command else {
        panic!("expected bench command");
    };
    assert_eq!(
        args.run.setting_args.setting,
        vec![(
            "studio_site_build_prompt_variant".to_string(),
            "astro-docs-content-collection".to_string()
        )]
    );
    assert!(
        !args.run.args.iter().any(|arg| arg == "--placement"),
        "--placement must not be forwarded to bench workloads"
    );
}
