use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use homeboy_core::error::{Error, Result};
pub use homeboy_extension_contract::trace_preview::{
    TracePreviewAssetCheck, TracePreviewAssetFanoutReport, TracePreviewAssetFanoutRequest,
    TracePreviewMetadata,
};
use homeboy_extension_contract::TracePublicPreviewSpec;

const DEFAULT_STARTUP_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_ASSET_FANOUT_CONCURRENCY: usize = 16;
const DEFAULT_ASSET_FANOUT_REPEAT_COUNT: usize = 1;

pub struct TracePublicPreviewSession {
    child: Option<Child>,
    metadata: TracePreviewMetadata,
}

impl TracePublicPreviewSession {
    pub fn start(spec: &TracePublicPreviewSpec) -> Result<Self> {
        let mut child = match spec.command.as_deref() {
            Some(command) if spec.public_origin.is_none() => Some(start_provider_command(command)?),
            _ => None,
        };
        let public_origin = match spec.public_origin.as_deref() {
            Some(origin) => origin.to_string(),
            None => read_public_origin(
                child.as_mut().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "public_preview",
                        "public_preview requires public_origin or command",
                        Some(spec.local_origin.clone()),
                        None,
                    )
                })?,
                spec.startup_timeout_seconds
                    .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECONDS),
            )?,
        };

        let is_https = public_origin.starts_with("https://");
        if spec.require_https && !is_https {
            stop_child(&mut child);
            return Err(Error::validation_invalid_argument(
                "public_preview.public_origin",
                "trace public_preview requires an HTTPS public origin",
                Some(public_origin),
                None,
            ));
        }

        let required_assets =
            stop_child_on_err(preflight_required_assets(spec, &public_origin), &mut child)?;

        let asset_fanout =
            stop_child_on_err(validate_asset_fanout(spec, &public_origin), &mut child)?;

        let process_id = child.as_ref().map(|child| child.id().to_string());
        Ok(Self {
            child,
            metadata: TracePreviewMetadata {
                schema: "homeboy/preview/v1".to_string(),
                requested_mode: "public-https".to_string(),
                provider: spec
                    .provider
                    .clone()
                    .unwrap_or_else(|| "external-command".to_string()),
                local_origin: spec.local_origin.clone(),
                local_url: spec.local_origin.clone(),
                public_origin: public_origin.clone(),
                public_url: public_origin.clone(),
                browser_effective_origin: public_origin.clone(),
                window_location_origin: public_origin,
                window_is_secure_context: is_https,
                require_https: spec.require_https,
                required_assets,
                asset_fanout,
                status: "running".to_string(),
                process_id,
                cleanup_status: "pending".to_string(),
            },
        })
    }

    pub fn metadata(&self) -> &TracePreviewMetadata {
        &self.metadata
    }

    pub fn env_vars(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![
            (
                homeboy_core::observation::context::PREVIEW_METADATA_ENV.to_string(),
                serde_json::to_string(&self.metadata).map_err(|e| {
                    Error::internal_json(
                        format!("Failed to serialize trace preview metadata: {e}"),
                        Some("trace.preview.serialize".to_string()),
                    )
                })?,
            ),
            (
                homeboy_core::observation::context::PREVIEW_PUBLIC_URL_ENV.to_string(),
                self.metadata.public_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_PREVIEW_LOCAL_ORIGIN".to_string(),
                self.metadata.local_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_PREVIEW_PUBLIC_ORIGIN".to_string(),
                self.metadata.public_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_BROWSER_EFFECTIVE_ORIGIN".to_string(),
                self.metadata.browser_effective_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_WINDOW_LOCATION_ORIGIN".to_string(),
                self.metadata.window_location_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_WINDOW_IS_SECURE_CONTEXT".to_string(),
                self.metadata.window_is_secure_context.to_string(),
            ),
            (
                "HOMEBOY_TRACE_PREVIEW_PROVIDER".to_string(),
                self.metadata.provider.clone(),
            ),
        ])
    }

    pub fn finish(mut self) -> TracePreviewMetadata {
        self.metadata.cleanup_status = if stop_child(&mut self.child) {
            "terminated".to_string()
        } else {
            "not_started".to_string()
        };
        self.metadata.status = "stopped".to_string();
        self.metadata.clone()
    }
}

impl Drop for TracePublicPreviewSession {
    fn drop(&mut self) {
        stop_child(&mut self.child);
    }
}

fn start_provider_command(command: &str) -> Result<Child> {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to start trace public_preview command: {e}"),
                Some("trace.preview.start".to_string()),
            )
        })
}

fn read_public_origin(child: &mut Child, timeout_seconds: u64) -> Result<String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::internal_io(
            "trace public_preview command stdout was not captured".to_string(),
            Some("trace.preview.stdout".to_string()),
        )
    })?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Some(origin) = first_https_origin(&line) {
                        let _ = sender.send(Ok(origin));
                        return;
                    }
                }
                Err(e) => {
                    let _ = sender.send(Err(e.to_string()));
                    return;
                }
            }
        }
        let _ = sender.send(Err(
            "provider stdout closed before an HTTPS URL appeared".to_string()
        ));
    });

    match receiver.recv_timeout(Duration::from_secs(timeout_seconds.max(1))) {
        Ok(Ok(origin)) => Ok(origin),
        Ok(Err(message)) => Err(Error::validation_invalid_argument(
            "public_preview.command",
            format!("trace public_preview command failed to provide an HTTPS URL: {message}"),
            None,
            None,
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            Err(Error::validation_invalid_argument(
                "public_preview.command",
                "trace public_preview command did not print an HTTPS URL before timeout",
                None,
                None,
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::validation_invalid_argument(
            "public_preview.command",
            "trace public_preview command stdout reader stopped before an HTTPS URL appeared",
            None,
            None,
        )),
    }
}

fn preflight_required_assets(
    spec: &TracePublicPreviewSpec,
    public_origin: &str,
) -> Result<Vec<TracePreviewAssetCheck>> {
    if spec.required_asset_paths.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to create trace public_preview asset preflight client: {e}"),
                Some("trace.preview.asset_client".to_string()),
            )
        })?;

    let mut checks = Vec::with_capacity(spec.required_asset_paths.len());
    for path in &spec.required_asset_paths {
        let url = preview_asset_url(public_origin, path);
        let check = match client.get(&url).send() {
            Ok(response) => {
                let status = response.status();
                TracePreviewAssetCheck {
                    path: path.clone(),
                    url,
                    status: Some(status.as_u16()),
                    ok: status.is_success(),
                    error: None,
                }
            }
            Err(error) => TracePreviewAssetCheck {
                path: path.clone(),
                url,
                status: None,
                ok: false,
                error: Some(error.to_string()),
            },
        };
        checks.push(check);
    }

    let failed: Vec<&TracePreviewAssetCheck> = checks.iter().filter(|check| !check.ok).collect();
    if failed.is_empty() {
        return Ok(checks);
    }

    let details = failed
        .iter()
        .map(|check| match (check.status, check.error.as_deref()) {
            (Some(status), _) => format!("{} -> HTTP {}", check.url, status),
            (None, Some(error)) => format!("{} -> {error}", check.url),
            (None, None) => format!("{} -> request failed", check.url),
        })
        .collect::<Vec<_>>()
        .join("; ");

    Err(Error::validation_invalid_argument(
        "public_preview.required_asset_paths",
        format!(
            "trace public_preview required asset preflight failed before trace collection: {details}"
        ),
        Some(public_origin.to_string()),
        Some(failed.iter().map(|check| check.url.clone()).collect()),
    ))
}

fn validate_asset_fanout(
    spec: &TracePublicPreviewSpec,
    public_origin: &str,
) -> Result<Option<TracePreviewAssetFanoutReport>> {
    let Some(fanout) = spec.asset_fanout.as_ref() else {
        return Ok(None);
    };
    if fanout.asset_paths.is_empty() {
        return Ok(Some(empty_asset_fanout_report(fanout)));
    }

    let concurrency = fanout
        .concurrency
        .unwrap_or(DEFAULT_ASSET_FANOUT_CONCURRENCY)
        .max(1);
    let repeat_count = fanout
        .repeat_count
        .unwrap_or(DEFAULT_ASSET_FANOUT_REPEAT_COUNT)
        .max(1);
    let expected_request_count = fanout.asset_paths.len() * repeat_count;
    let mut jobs = VecDeque::with_capacity(expected_request_count);
    for _ in 0..repeat_count {
        for path in &fanout.asset_paths {
            jobs.push_back((path.clone(), preview_asset_url(public_origin, path)));
        }
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to create trace public_preview asset fanout client: {e}"),
                Some("trace.preview.asset_fanout_client".to_string()),
            )
        })?;
    let jobs = Arc::new(Mutex::new(jobs));
    let requests = Arc::new(Mutex::new(Vec::with_capacity(expected_request_count)));
    let expected_body_contains = fanout.expected_body_contains.clone();

    let worker_count = concurrency.min(expected_request_count);
    let mut workers = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let client = client.clone();
        let jobs = Arc::clone(&jobs);
        let requests = Arc::clone(&requests);
        let expected_body_contains = expected_body_contains.clone();
        workers.push(std::thread::spawn(move || loop {
            let Some((path, url)) = jobs.lock().expect("asset fanout jobs lock").pop_front() else {
                break;
            };
            let request = fetch_fanout_asset(&client, path, url, expected_body_contains.as_deref());
            requests
                .lock()
                .expect("asset fanout requests lock")
                .push(request);
        }));
    }
    for worker in workers {
        worker.join().map_err(|_| {
            Error::internal_io(
                "trace public_preview asset fanout worker panicked".to_string(),
                Some("trace.preview.asset_fanout".to_string()),
            )
        })?;
    }

    let requests = Arc::try_unwrap(requests)
        .expect("asset fanout request handles released")
        .into_inner()
        .expect("asset fanout requests lock");
    let report = asset_fanout_report(
        concurrency,
        repeat_count,
        fanout.asset_paths.len(),
        requests,
    );
    if report.failure_count == 0 && report.client_request_count == expected_request_count {
        return Ok(Some(report));
    }

    let details = report
        .failure_buckets
        .iter()
        .map(|(bucket, count)| format!("{bucket}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::validation_invalid_argument(
        "public_preview.asset_fanout",
        format!(
            "trace public_preview asset fanout failed before trace collection: expected_request_count={} client_request_count={} failure_count={} failure_buckets=[{}]",
            report.expected_request_count,
            report.client_request_count,
            report.failure_count,
            details
        ),
        Some(public_origin.to_string()),
        Some(report.failure_buckets.keys().cloned().collect()),
    ))
}

fn fetch_fanout_asset(
    client: &reqwest::blocking::Client,
    path: String,
    url: String,
    expected_body_contains: Option<&str>,
) -> TracePreviewAssetFanoutRequest {
    match client.get(&url).send() {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                let failure_bucket = format!("http_{}", status.as_u16());
                return TracePreviewAssetFanoutRequest {
                    path,
                    url,
                    status: Some(status.as_u16()),
                    ok: false,
                    failure_bucket: Some(failure_bucket),
                    error: None,
                };
            }
            if let Some(expected) = expected_body_contains {
                match response.text() {
                    Ok(body) if body.contains(expected) => TracePreviewAssetFanoutRequest {
                        path,
                        url,
                        status: Some(status.as_u16()),
                        ok: true,
                        failure_bucket: None,
                        error: None,
                    },
                    Ok(_) => TracePreviewAssetFanoutRequest {
                        path,
                        url,
                        status: Some(status.as_u16()),
                        ok: false,
                        failure_bucket: Some("body_mismatch".to_string()),
                        error: Some("response body did not contain expected text".to_string()),
                    },
                    Err(error) => TracePreviewAssetFanoutRequest {
                        path,
                        url,
                        status: Some(status.as_u16()),
                        ok: false,
                        failure_bucket: Some("body_read_error".to_string()),
                        error: Some(error.to_string()),
                    },
                }
            } else {
                TracePreviewAssetFanoutRequest {
                    path,
                    url,
                    status: Some(status.as_u16()),
                    ok: true,
                    failure_bucket: None,
                    error: None,
                }
            }
        }
        Err(error) => TracePreviewAssetFanoutRequest {
            path,
            url,
            status: None,
            ok: false,
            failure_bucket: Some(request_error_bucket(&error)),
            error: Some(error.to_string()),
        },
    }
}

fn request_error_bucket(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timeout".to_string();
    }
    if error.is_connect() {
        return "connect_error".to_string();
    }
    if error.is_body() {
        return "body_error".to_string();
    }
    if error.is_decode() {
        return "decode_error".to_string();
    }
    "request_error".to_string()
}

fn asset_fanout_report(
    concurrency: usize,
    repeat_count: usize,
    asset_path_count: usize,
    requests: Vec<TracePreviewAssetFanoutRequest>,
) -> TracePreviewAssetFanoutReport {
    let expected_request_count = asset_path_count * repeat_count;
    let client_request_count = requests.len();
    let success_count = requests.iter().filter(|request| request.ok).count();
    let failure_count = client_request_count.saturating_sub(success_count);
    let mut status_counts = BTreeMap::new();
    let mut failure_buckets = BTreeMap::new();
    for request in &requests {
        let status = request
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "none".to_string());
        *status_counts.entry(status).or_insert(0) += 1;
        if let Some(bucket) = request.failure_bucket.as_ref() {
            *failure_buckets.entry(bucket.clone()).or_insert(0) += 1;
        }
    }

    TracePreviewAssetFanoutReport {
        schema: "homeboy/preview-asset-fanout/v1".to_string(),
        concurrency,
        repeat_count,
        asset_path_count,
        expected_request_count,
        client_request_count,
        success_count,
        failure_count,
        ingress_request_count: None,
        local_origin_request_count: None,
        status_counts,
        failure_buckets,
        requests,
    }
}

fn empty_asset_fanout_report(
    fanout: &homeboy_extension_contract::TracePreviewAssetFanoutSpec,
) -> TracePreviewAssetFanoutReport {
    TracePreviewAssetFanoutReport {
        schema: "homeboy/preview-asset-fanout/v1".to_string(),
        concurrency: fanout
            .concurrency
            .unwrap_or(DEFAULT_ASSET_FANOUT_CONCURRENCY)
            .max(1),
        repeat_count: fanout
            .repeat_count
            .unwrap_or(DEFAULT_ASSET_FANOUT_REPEAT_COUNT)
            .max(1),
        asset_path_count: 0,
        expected_request_count: 0,
        client_request_count: 0,
        success_count: 0,
        failure_count: 0,
        ingress_request_count: None,
        local_origin_request_count: None,
        status_counts: BTreeMap::new(),
        failure_buckets: BTreeMap::new(),
        requests: Vec::new(),
    }
}

fn preview_asset_url(public_origin: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{}/{}",
        public_origin.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn first_https_origin(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')'))
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches('/').to_string())
}

/// Unwrap `result`, stopping the preview `child` process before propagating
/// any error so a failed preflight/validation step never leaks the child.
fn stop_child_on_err<T>(result: Result<T>, child: &mut Option<Child>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            stop_child(child);
            Err(error)
        }
    }
}

fn stop_child(child: &mut Option<Child>) -> bool {
    let Some(mut child) = child.take() else {
        return false;
    };
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    true
}

#[cfg(test)]
mod tests {
    use super::{first_https_origin, TracePublicPreviewSession};
    use homeboy_extension_contract::{
        TracePreviewAssetFanoutSpec, TracePublicPreviewMode, TracePublicPreviewSpec,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn extracts_first_https_origin_from_provider_output() {
        assert_eq!(
            first_https_origin("ready: https://preview.example.test/run-1\n").as_deref(),
            Some("https://preview.example.test/run-1")
        );
    }

    #[test]
    fn starts_provider_command_and_records_preview_metadata() {
        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: "http://127.0.0.1:8080".to_string(),
            public_origin: None,
            command: Some(
                "printf 'ready https://preview.example.test/run-1\\n'; sleep 5".to_string(),
            ),
            require_https: true,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: Some(2),
            required_asset_paths: Vec::new(),
            asset_fanout: None,
        })
        .expect("preview starts");

        assert_eq!(
            session.metadata().public_origin,
            "https://preview.example.test/run-1"
        );
        assert!(session.metadata().window_is_secure_context);

        let metadata = session.finish();
        assert_eq!(metadata.status, "stopped");
        assert_eq!(metadata.cleanup_status, "terminated");
    }

    #[test]
    fn records_successful_required_asset_preflight() {
        let origin =
            start_asset_fixture_server(vec![("/assets/frontend.js?ver=1".to_string(), 200)]);

        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: vec!["/assets/frontend.js?ver=1".to_string()],
            asset_fanout: None,
        })
        .expect("preview starts after asset preflight");

        assert_eq!(session.metadata().required_assets.len(), 1);
        assert!(session.metadata().required_assets[0].ok);
        assert_eq!(session.metadata().required_assets[0].status, Some(200));
    }

    #[test]
    fn fails_fast_when_required_asset_cannot_be_fetched() {
        let origin =
            start_asset_fixture_server(vec![("/assets/frontend.js?ver=1".to_string(), 502)]);

        let result = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: vec!["/assets/frontend.js?ver=1".to_string()],
            asset_fanout: None,
        });

        let error = match result {
            Ok(_) => panic!("required asset preflight should fail before trace starts"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("required asset preflight failed before trace collection"));
        assert!(message.contains("/assets/frontend.js?ver=1 -> HTTP 502"));
    }

    #[test]
    fn records_successful_concurrent_asset_fanout() {
        let paths = vec![
            "/assets/app.js?ver=1".to_string(),
            "/assets/app.css?ver=1".to_string(),
            "/assets/vendor.js?ver=1".to_string(),
            "/assets/runtime.css?ver=1".to_string(),
        ];
        let expected_requests = paths.len() * 3;
        let (origin, local_origin_count) =
            start_fanout_fixture_server(paths.clone(), expected_requests, 200);

        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("homeboy-native-fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: Vec::new(),
            asset_fanout: Some(TracePreviewAssetFanoutSpec {
                asset_paths: paths,
                concurrency: Some(4),
                repeat_count: Some(3),
                expected_body_contains: Some("homeboy-fanout-ok".to_string()),
            }),
        })
        .expect("preview starts after asset fanout");

        let report = session
            .metadata()
            .asset_fanout
            .as_ref()
            .expect("fanout report");
        assert_eq!(report.schema, "homeboy/preview-asset-fanout/v1");
        assert_eq!(report.concurrency, 4);
        assert_eq!(report.repeat_count, 3);
        assert_eq!(report.expected_request_count, expected_requests);
        assert_eq!(report.client_request_count, expected_requests);
        assert_eq!(report.success_count, expected_requests);
        assert_eq!(report.failure_count, 0);
        assert_eq!(report.status_counts.get("200"), Some(&expected_requests));
        assert!(report.failure_buckets.is_empty());
        assert_eq!(local_origin_count.load(Ordering::SeqCst), expected_requests);
    }

    #[test]
    fn fails_fast_when_concurrent_asset_fanout_sees_bad_gateway() {
        let paths = vec![
            "/assets/app.js?ver=1".to_string(),
            "/assets/app.css?ver=1".to_string(),
        ];
        let (origin, local_origin_count) =
            start_fanout_fixture_server(paths.clone(), paths.len(), 502);

        let result = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("homeboy-native-fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: Vec::new(),
            asset_fanout: Some(TracePreviewAssetFanoutSpec {
                asset_paths: paths,
                concurrency: Some(2),
                repeat_count: Some(1),
                expected_body_contains: None,
            }),
        });

        let error = match result {
            Ok(_) => panic!("asset fanout should fail before trace starts"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("asset fanout failed before trace collection"));
        assert!(message.contains("failure_buckets=[http_502=2]"));
        assert_eq!(local_origin_count.load(Ordering::SeqCst), 2);
    }

    fn start_asset_fixture_server(routes: Vec<(String, u16)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let addr = listener.local_addr().expect("fixture server addr");
        thread::spawn(move || {
            for stream in listener.incoming().take(routes.len()) {
                let mut stream = stream.expect("fixture connection");
                let mut buffer = [0; 1024];
                let len = stream.read(&mut buffer).expect("read request");
                let request = String::from_utf8_lossy(&buffer[..len]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let status = routes
                    .iter()
                    .find(|(path, _)| path == target)
                    .map(|(_, status)| *status)
                    .unwrap_or(404);
                let reason = if status == 200 { "OK" } else { "Bad Gateway" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        format!("http://{addr}")
    }

    fn start_fanout_fixture_server(
        paths: Vec<String>,
        expected_requests: usize,
        status: u16,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fanout fixture server");
        let addr = listener.local_addr().expect("fanout fixture server addr");
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        thread::spawn(move || {
            for stream in listener.incoming().take(expected_requests) {
                let mut stream = stream.expect("fixture connection");
                server_request_count.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0; 2048];
                let len = stream.read(&mut buffer).expect("read request");
                let request = String::from_utf8_lossy(&buffer[..len]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let route_status = if paths.iter().any(|path| path == target) {
                    status
                } else {
                    404
                };
                let reason = if route_status == 200 {
                    "OK"
                } else {
                    "Bad Gateway"
                };
                let body = "homeboy-fanout-ok";
                let response = format!(
                    "HTTP/1.1 {route_status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        (format!("http://{addr}"), request_count)
    }
}
