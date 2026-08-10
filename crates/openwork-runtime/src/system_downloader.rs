use crate::{CancellationToken, DownloadReceipt, DownloadRequest, Downloader, RuntimeResult};
use openwork_core::{ErrorCode, OpenWorkError};
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::time::Duration;

const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;
const BUFFER_BYTES: usize = 64 * 1024;

/// Network policy applied before a runtime artifact is fetched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadPolicy {
    allowed_hosts: BTreeSet<String>,
    max_bytes: u64,
    max_redirects: usize,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self::official_runtime_sources()
    }
}

impl DownloadPolicy {
    /// Restricts bootstrap downloads to the two upstream-owned installer hosts.
    #[must_use]
    pub fn official_runtime_sources() -> Self {
        Self {
            allowed_hosts: BTreeSet::from(["chatgpt.com".to_owned(), "claude.ai".to_owned()]),
            max_bytes: DEFAULT_MAX_DOWNLOAD_BYTES,
            max_redirects: 3,
        }
    }

    /// Constructs a policy for an explicitly reviewed set of exact DNS names.
    #[must_use]
    pub fn new(
        allowed_hosts: impl IntoIterator<Item = String>,
        max_bytes: u64,
        max_redirects: usize,
    ) -> Self {
        Self {
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|host| host.to_ascii_lowercase())
                .collect(),
            max_bytes,
            max_redirects,
        }
    }

    fn validate_url(&self, url: &Url) -> RuntimeResult<()> {
        if url.scheme() != "https" {
            return Err(download_error(
                "runtime downloads require HTTPS",
                "Use an upstream-owned HTTPS installer URL.",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(download_error(
                "runtime download URLs must not contain credentials",
                "Remove user information from the installer URL.",
            ));
        }
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !self.allowed_hosts.contains(&host) {
            return Err(download_error(
                format!("runtime download host `{host}` is not allowlisted"),
                "Review the upstream source and add its exact host to DownloadPolicy.",
            ));
        }
        Ok(())
    }

    fn validate_redirect(&self, previous_urls: usize, url: &Url) -> RuntimeResult<()> {
        if previous_urls > self.max_redirects {
            return Err(download_error(
                "runtime download exceeded the redirect limit",
                "Use the final reviewed upstream HTTPS installer URL.",
            ));
        }
        self.validate_url(url)
    }
}

/// Blocking HTTPS downloader used by the managed installer executor.
///
/// Files are streamed to a restrictive temporary file in the destination
/// directory and only become visible after verification succeeds. Existing
/// destinations are never replaced.
#[derive(Clone, Debug)]
pub struct SystemDownloader {
    client: Client,
    policy: DownloadPolicy,
}

impl SystemDownloader {
    /// Builds a downloader with the official-source allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform HTTPS client cannot be initialized.
    pub fn new() -> RuntimeResult<Self> {
        Self::with_policy(DownloadPolicy::default())
    }

    /// Builds a downloader with an explicitly reviewed policy.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform HTTPS client cannot be initialized.
    pub fn with_policy(policy: DownloadPolicy) -> RuntimeResult<Self> {
        let redirect_policy = policy.clone();
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::custom(move |attempt| {
                if let Err(error) =
                    redirect_policy.validate_redirect(attempt.previous().len(), attempt.url())
                {
                    return attempt.error(std::io::Error::other(error.message));
                }
                attempt.follow()
            }))
            .build()
            .map_err(|_| {
                download_error(
                    "failed to initialize the HTTPS client",
                    "Verify the platform TLS trust store.",
                )
            })?;
        Ok(Self { client, policy })
    }

    fn validate_request<'a>(
        &self,
        request: &'a DownloadRequest,
    ) -> RuntimeResult<(Url, Option<String>, &'a std::path::Path)> {
        let url = Url::parse(&request.url).map_err(|_| {
            download_error(
                "runtime download URL is invalid",
                "Use a complete upstream HTTPS URL.",
            )
        })?;
        self.policy.validate_url(&url)?;
        let expected = validate_expected_digest(request.expected_sha256.as_deref())?;
        if request.destination.exists() {
            return Err(existing_destination_error(&request.destination));
        }
        let parent = request.destination.parent().ok_or_else(|| {
            download_error(
                "download destination has no parent directory",
                "Choose a managed cache destination.",
            )
        })?;
        Ok((url, expected, parent))
    }

    fn send(&self, url: Url, timeout_millis: u64) -> RuntimeResult<reqwest::blocking::Response> {
        let response = self
            .client
            .get(url)
            .timeout(Duration::from_millis(timeout_millis))
            .send()
            .map_err(|_| {
                download_error(
                    "runtime download request failed",
                    "Check network access, TLS trust, and the upstream service status.",
                )
            })?
            .error_for_status()
            .map_err(|_| {
                download_error(
                    "runtime download returned an unsuccessful HTTP status",
                    "Check the official upstream installer URL.",
                )
            })?;
        if response
            .content_length()
            .is_some_and(|length| length > self.policy.max_bytes)
        {
            return Err(size_limit_error());
        }
        Ok(response)
    }

    fn stream_to_temporary(
        &self,
        mut response: reqwest::blocking::Response,
        parent: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> RuntimeResult<(tempfile::NamedTempFile, u64, String)> {
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        let mut hasher = Sha256::new();
        let mut bytes_written = 0_u64;
        let mut buffer = vec![0_u8; BUFFER_BYTES].into_boxed_slice();
        loop {
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let count = response.read(&mut buffer).map_err(|_| {
                download_error(
                    "runtime download stream failed",
                    "Retry after checking network stability.",
                )
            })?;
            if count == 0 {
                break;
            }
            bytes_written = bytes_written
                .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
                .ok_or_else(|| {
                    download_error("runtime download size overflow", "Abort the install.")
                })?;
            if bytes_written > self.policy.max_bytes {
                return Err(size_limit_error());
            }
            temporary.write_all(&buffer[..count]).map_err(io_error)?;
            hasher.update(&buffer[..count]);
        }
        temporary.as_file().sync_all().map_err(io_error)?;
        Ok((temporary, bytes_written, encode_hex(&hasher.finalize())))
    }
}

impl Downloader for SystemDownloader {
    fn download(
        &self,
        request: &DownloadRequest,
        cancellation: &CancellationToken,
    ) -> RuntimeResult<DownloadReceipt> {
        if cancellation.is_cancelled() {
            return Err(cancelled_error());
        }
        let (url, expected, parent) = self.validate_request(request)?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let response = self.send(url, request.timeout_millis)?;
        let (temporary, bytes_written, observed) =
            self.stream_to_temporary(response, parent, cancellation)?;
        let verified = expected
            .as_ref()
            .is_some_and(|expected| expected == &observed);
        if expected.is_some() && !verified {
            return Err(download_error(
                "runtime download SHA-256 verification failed",
                "Discard the artifact and verify the checksum against the official release.",
            ));
        }
        temporary
            .persist_noclobber(&request.destination)
            .map_err(|_| {
                download_error(
                    format!(
                        "refusing to overwrite existing download `{}`",
                        request.destination.display()
                    ),
                    "Review the destination and retry with an unused managed path.",
                )
            })?;
        sync_parent(parent)?;

        Ok(DownloadReceipt {
            bytes_written,
            observed_sha256: observed,
            verified,
        })
    }
}

fn validate_expected_digest(value: Option<&str>) -> RuntimeResult<Option<String>> {
    value
        .map(|digest| {
            let normalized = digest.trim().to_ascii_lowercase();
            if normalized.len() == 64 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Ok(normalized)
            } else {
                Err(download_error(
                    "expected SHA-256 digest is invalid",
                    "Provide exactly 64 hexadecimal characters from the official release.",
                ))
            }
        })
        .transpose()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(unix)]
fn sync_parent(parent: &std::path::Path) -> RuntimeResult<()> {
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
fn sync_parent(_: &std::path::Path) -> RuntimeResult<()> {
    Ok(())
}

fn cancelled_error() -> OpenWorkError {
    download_error(
        "runtime download was cancelled",
        "Retry the install when ready.",
    )
}

fn existing_destination_error(destination: &std::path::Path) -> OpenWorkError {
    download_error(
        format!(
            "refusing to overwrite existing download `{}`",
            destination.display()
        ),
        "Remove the stale managed download after reviewing it, then retry.",
    )
}

fn size_limit_error() -> OpenWorkError {
    download_error(
        "runtime download exceeds the configured size limit",
        "Review the upstream artifact and increase the explicit policy limit if valid.",
    )
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: std::io::Error) -> OpenWorkError {
    download_error(
        format!("runtime download filesystem operation failed: {error}"),
        "Check free space and permissions for the managed cache directory.",
    )
}

fn download_error(message: impl Into<String>, remediation: impl Into<String>) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::InstallFailed, message).with_remediation(remediation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_policy_accepts_only_reviewed_https_hosts() {
        let policy = DownloadPolicy::default();
        assert!(
            policy
                .validate_url(&Url::parse("https://claude.ai/install.sh").unwrap())
                .is_ok()
        );
        assert!(
            policy
                .validate_url(&Url::parse("http://claude.ai/install.sh").unwrap())
                .is_err()
        );
        assert!(
            policy
                .validate_url(&Url::parse("https://claude.ai.evil.test/install.sh").unwrap())
                .is_err()
        );
        assert!(
            policy
                .validate_url(&Url::parse("https://user:secret@claude.ai/install.sh").unwrap())
                .is_err()
        );
    }

    #[test]
    fn redirects_must_remain_on_allowlisted_https_hosts() {
        let policy = DownloadPolicy::default();
        assert!(
            policy
                .validate_redirect(
                    1,
                    &Url::parse("https://chatgpt.com/codex/install.sh").unwrap()
                )
                .is_ok()
        );
        assert!(
            policy
                .validate_redirect(1, &Url::parse("https://evil.test/install.sh").unwrap())
                .is_err()
        );
        assert!(
            policy
                .validate_redirect(
                    1,
                    &Url::parse("http://chatgpt.com/codex/install.sh").unwrap()
                )
                .is_err()
        );
    }

    #[test]
    fn redirect_limit_is_an_error_instead_of_a_successful_response() {
        let policy = DownloadPolicy::new(["example.test".to_owned()], 1024, 2);
        let destination = Url::parse("https://example.test/final").unwrap();
        assert!(policy.validate_redirect(2, &destination).is_ok());
        let error = policy.validate_redirect(3, &destination).unwrap_err();
        assert!(error.message.contains("redirect limit"));
    }

    #[test]
    fn malformed_expected_digest_is_rejected_before_network_access() {
        let downloader = SystemDownloader::new().unwrap();
        let request = DownloadRequest {
            url: "https://claude.ai/install.sh".to_owned(),
            destination: PathBuf::from("never-created"),
            expected_sha256: Some("not-a-digest".to_owned()),
            timeout_millis: 1,
        };
        let error = downloader
            .download(&request, &CancellationToken::new())
            .unwrap_err();
        assert!(error.message.contains("SHA-256"));
        assert!(!request.destination.exists());
    }

    #[test]
    fn digest_encoding_is_lowercase_and_fixed_width() {
        assert_eq!(encode_hex(&[0x00, 0x09, 0xaf, 0xff]), "0009afff");
    }

    #[test]
    fn cancellation_is_rejected_before_network_access() {
        let downloader = SystemDownloader::new().unwrap();
        let request = DownloadRequest {
            url: "https://claude.ai/install.sh".to_owned(),
            destination: PathBuf::from("never-created"),
            expected_sha256: None,
            timeout_millis: 1,
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = downloader.download(&request, &cancellation).unwrap_err();
        assert!(error.message.contains("cancelled"));
        assert!(!request.destination.exists());
    }
}
