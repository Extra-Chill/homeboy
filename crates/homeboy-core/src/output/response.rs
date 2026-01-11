use crate::Error;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CliResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CliError>,
}

#[derive(Debug, Serialize)]
pub struct CliError {
    pub code: String,
    pub message: String,
}

impl<T: Serialize> CliResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            r#"{"success":false,"error":{"code":"JSON_ERROR","message":"Failed to serialize response"}}"#.to_string()
        })
    }
}

impl CliResponse<()> {
    pub fn failure(code: &str, message: &str) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(CliError {
                code: code.to_string(),
                message: message.to_string(),
            }),
        }
    }

    pub fn from_error(err: &Error) -> Self {
        Self::failure(err.code(), &err.to_string())
    }
}

pub fn print_success<T: Serialize>(data: T) {
    println!("{}", CliResponse::success(data).to_json());
}

pub fn print_error(code: &str, message: &str) {
    println!("{}", CliResponse::<()>::failure(code, message).to_json());
}

pub fn print_result<T: Serialize>(result: crate::Result<T>) {
    match result {
        Ok(data) => print_success(data),
        Err(err) => println!("{}", CliResponse::<()>::from_error(&err).to_json()),
    }
}
