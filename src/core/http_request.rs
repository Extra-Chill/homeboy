use crate::core::error::{Error, Result};
use crate::core::server::{auth_profiles, http};
use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct HttpRequestInput {
    pub method: String,
    pub url: String,
    pub proxy_url: Option<String>,
    pub auth_profile: Option<String>,
    pub headers: Vec<String>,
    pub json_body: Option<String>,
    pub form_body: Vec<(String, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpRequestOutput {
    pub variant: String,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub headers: BTreeMap<String, Vec<String>>,
    pub body: Value,
}

pub fn run(input: HttpRequestInput) -> Result<HttpRequestOutput> {
    let method = input.method.parse::<Method>().map_err(|e| {
        Error::validation_invalid_argument(
            "method",
            e.to_string(),
            Some(input.method.clone()),
            None,
        )
    })?;
    let client = http::build_client_with_proxy(input.proxy_url.as_deref())?;
    let mut request = client.request(method.clone(), &input.url);

    let headers = build_headers(&input)?;
    if !headers.is_empty() {
        request = request.headers(headers);
    }

    if let Some(raw_json) = input.json_body.as_deref() {
        let value: Value = serde_json::from_str(raw_json)
            .map_err(|e| Error::validation_invalid_argument("json", e.to_string(), None, None))?;
        request = request.json(&value);
    } else if !input.form_body.is_empty() {
        request = request.form(&input.form_body);
    }

    let response = request.send().map_err(http::http_error)?;
    response_output(method.as_str(), &input.url, response)
}

fn build_headers(input: &HttpRequestInput) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();

    if let Some(profile) = input.auth_profile.as_deref() {
        let value = auth_profiles::profile_authorization_header(profile)?;
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&value).map_err(|e| {
                Error::validation_invalid_argument("auth-profile", e.to_string(), None, None)
            })?,
        );
    }

    if input.json_body.is_some() {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    for header in &input.headers {
        let (name, value) = parse_header(header)?;
        headers.insert(name, value);
    }

    Ok(headers)
}

fn parse_header(header: &str) -> Result<(HeaderName, HeaderValue)> {
    let Some((name, value)) = header.split_once(':') else {
        return Err(Error::validation_invalid_argument(
            "header",
            "Headers must use 'Name: value' format",
            Some(header.to_string()),
            None,
        ));
    };

    let name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|e| {
        Error::validation_invalid_argument("header", e.to_string(), Some(header.to_string()), None)
    })?;
    let value = HeaderValue::from_str(value.trim()).map_err(|e| {
        Error::validation_invalid_argument("header", e.to_string(), Some(header.to_string()), None)
    })?;
    Ok((name, value))
}

fn response_output(method: &str, url: &str, response: Response) -> Result<HttpRequestOutput> {
    let status = response.status().as_u16();
    let headers = response_headers(response.headers());
    let text = response.text().map_err(http::http_error)?;
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));

    Ok(HttpRequestOutput {
        variant: "response".to_string(),
        method: method.to_string(),
        url: url.to_string(),
        status,
        headers,
        body,
    })
}

fn response_headers(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        out.entry(name.as_str().to_string())
            .or_default()
            .push(value.to_str().unwrap_or_default().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ErrorCode;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn test_run() {
        get_returns_status_headers_and_json_body();
    }

    #[test]
    fn get_returns_status_headers_and_json_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let output = run(HttpRequestInput {
            method: "GET".to_string(),
            url: format!("http://{}", addr),
            proxy_url: None,
            auth_profile: None,
            headers: Vec::new(),
            json_body: None,
            form_body: Vec::new(),
        })
        .unwrap();

        assert_eq!(output.variant, "response");
        assert_eq!(output.status, 200);
        assert_eq!(output.body["ok"], true);
        assert_eq!(output.headers["content-type"], vec!["application/json"]);
    }

    #[test]
    fn invalid_header_is_rejected() {
        let err = build_headers(&HttpRequestInput {
            method: "GET".to_string(),
            url: "https://example.com".to_string(),
            proxy_url: None,
            auth_profile: None,
            headers: vec!["no-colon".to_string()],
            json_body: None,
            form_body: Vec::new(),
        })
        .unwrap_err();

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    }
}
