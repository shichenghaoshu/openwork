//! Shared result, error, event, and redaction primitives for `OpenWork`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// The stable product name used by every renderer.
pub const PRODUCT_NAME: &str = "OpenWork";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArguments,
    UnsupportedPlatform,
    PreflightFailed,
    RuntimeNotFound,
    RuntimeUnhealthy,
    InstallFailed,
    ConfigInvalid,
    InvalidStateTransition,
    PolicyDenied,
    ApprovalRequired,
    ApprovalInvalid,
    SandboxUnavailable,
    ExecutionFailed,
    RunTimedOut,
    RunCancelled,
    ArtifactInvalid,
    Io,
    Internal,
}

impl ErrorCode {
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::InvalidArguments => 2,
            Self::UnsupportedPlatform => 10,
            Self::PreflightFailed => 11,
            Self::RuntimeNotFound => 20,
            Self::RuntimeUnhealthy => 21,
            Self::InstallFailed => 30,
            Self::ConfigInvalid => 40,
            Self::InvalidStateTransition => 50,
            Self::PolicyDenied => 51,
            Self::ApprovalRequired => 52,
            Self::ApprovalInvalid => 53,
            Self::SandboxUnavailable => 60,
            Self::ExecutionFailed => 61,
            Self::RunTimedOut => 62,
            Self::RunCancelled => 63,
            Self::ArtifactInvalid => 64,
            Self::Io => 74,
            Self::Internal => 70,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenWorkError {
    pub code: ErrorCode,
    pub message: String,
    pub remediation: Option<String>,
}

impl OpenWorkError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: redact_text(&message.into()),
            remediation: None,
        }
    }

    #[must_use]
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(redact_text(&remediation.into()));
        self
    }

    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.code.exit_code()
    }
}

impl fmt::Display for OpenWorkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(remediation) = &self.remediation {
            write!(formatter, "; remediation: {remediation}")?;
        }
        Ok(())
    }
}

impl std::error::Error for OpenWorkError {}

/// Redacts common credential assignments and token prefixes from diagnostic text.
#[must_use]
pub fn redact_text(input: &str) -> String {
    let mut redact_following = 0_usize;
    input
        .split_whitespace()
        .map(|word| {
            if redact_following > 0 {
                redact_following -= 1;
                return "[REDACTED]".to_owned();
            }
            if let Some(following) = secret_assignment_tail(word) {
                redact_following = following;
                "[REDACTED]".to_owned()
            } else {
                redact_word(word)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Redacts secret-bearing fields recursively before structured data reaches logs or audit.
#[must_use]
pub fn redact_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(redact_json).collect()),
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| {
                    let redacted = if is_secret_key(key) {
                        Value::String("[REDACTED]".to_owned())
                    } else {
                        redact_json(value)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        Value::String(text) => Value::String(redact_text(text)),
        scalar => scalar.clone(),
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized.ends_with("sha256") || normalized.ends_with("hash") {
        return false;
    }
    [
        "authorization",
        "cookie",
        "setcookie",
        "password",
        "secret",
        "token",
        "apikey",
        "accesskey",
        "privatekey",
        "clientsecret",
        "prompt",
        "stdout",
        "stderr",
        "runtimeoutput",
        "rawoutput",
        "vendorpayload",
        "messagecontent",
        "message",
        "content",
        "parameters",
    ]
    .iter()
    .any(|secret| normalized.contains(secret))
}

fn secret_assignment_tail(word: &str) -> Option<usize> {
    let lower = word.to_ascii_lowercase();
    let keys = [
        "authorization",
        "set-cookie",
        "cookie",
        "password",
        "client_secret",
        "clientsecret",
        "private_key",
        "privatekey",
        "access_key",
        "accesskey",
        "api_key",
        "apikey",
        "secret",
        "token",
    ];
    for key in keys {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(key) {
            let end = offset + relative + key.len();
            let suffix = lower[end..].trim_start_matches(['"', '\'', ' ', '\t']);
            let Some(separator) = suffix.chars().next() else {
                break;
            };
            if separator == '=' || separator == ':' {
                let value = suffix[separator.len_utf8()..]
                    .trim_matches(['"', '\'', ',', ';', '}', ']', ' ']);
                let following = if key == "authorization" {
                    if value.is_empty() { 2 } else { 1 }
                } else {
                    usize::from(value.is_empty())
                };
                return Some(following);
            }
            offset = end;
        }
    }
    None
}

fn redact_word(word: &str) -> String {
    const SECRET_KEYS: [&str; 6] = [
        "TOKEN",
        "API_KEY",
        "PASSWORD",
        "SECRET",
        "AUTHORIZATION",
        "COOKIE",
    ];
    let upper = word.to_ascii_uppercase();
    if SECRET_KEYS
        .iter()
        .any(|key| upper.starts_with(&format!("{key}=")))
        || ["sk-", "ghp_", "gho_", "github_pat_"]
            .iter()
            .any(|prefix| word.starts_with(prefix))
    {
        "[REDACTED]".to_owned()
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ErrorCode::InvalidArguments.exit_code(), 2);
        assert_eq!(ErrorCode::UnsupportedPlatform.exit_code(), 10);
        assert_eq!(ErrorCode::RuntimeNotFound.exit_code(), 20);
        assert_eq!(ErrorCode::PolicyDenied.exit_code(), 51);
        assert_eq!(ErrorCode::SandboxUnavailable.exit_code(), 60);
        assert_eq!(ErrorCode::Internal.exit_code(), 70);
    }

    #[test]
    fn errors_redact_credentials() {
        let error = OpenWorkError::new(ErrorCode::RuntimeUnhealthy, "TOKEN=visible failed")
            .with_remediation("retry with sk-not-a-real-secret");
        assert_eq!(error.message, "[REDACTED] failed");
        assert!(!error.to_string().contains("not-a-real-secret"));
    }

    #[test]
    fn structured_redaction_handles_nested_headers_and_credentials() {
        let value = serde_json::json!({
            "headers": {"Authorization": "Bearer visible", "Cookie": "session=visible"},
            "nested": [{"client_secret": "visible"}],
            "stdout": "raw enterprise output",
            "prompt_sha256": "safe-digest",
            "safe": "kept"
        });
        let redacted = redact_json(&value);
        assert_eq!(redacted["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(redacted["headers"]["Cookie"], "[REDACTED]");
        assert_eq!(redacted["nested"][0]["client_secret"], "[REDACTED]");
        assert_eq!(redacted["stdout"], "[REDACTED]");
        assert_eq!(redacted["prompt_sha256"], "safe-digest");
        assert_eq!(redacted["safe"], "kept");
    }

    #[test]
    fn free_form_redaction_covers_headers_assignments_and_query_tokens() {
        let text = "Authorization: Bearer visible password: visible";
        let redacted = redact_text(text);
        assert!(!redacted.contains("Bearer"));
        assert!(!redacted.contains("visible"));

        let url = "https://example.invalid/?token=visible safe";
        let redacted_url = redact_text(url);
        assert!(!redacted_url.contains("visible"));
        assert!(redacted_url.ends_with("safe"));
    }
}
