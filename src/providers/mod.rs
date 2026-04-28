use anyhow::{Context, Result};
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

/// A live model with its current quota status.
#[derive(Debug, Clone)]
pub struct LiveModel {
    pub name: String,
    pub quota_percent: Option<u8>,
}

/// Build an HTTP client with the given timeout.
pub fn build_http_client(timeout_secs: u64) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("failed to build HTTP client")
}

/// Send a request and return the response, checking for HTTP errors.
pub fn send_request(
    request: reqwest::blocking::RequestBuilder,
    provider: &str,
) -> Result<Response> {
    request
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("{provider} request failed"))
}

/// Parse a JSON response body, returning a descriptive error on failure.
pub fn parse_json_response(response: Response, provider: &str) -> Result<Value> {
    response
        .json::<Value>()
        .with_context(|| format!("{provider} response was not valid JSON"))
}

/// Convert a percentage value to a u8 clamped to 0–100.
pub fn percent_to_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

/// Return the user's home directory.
pub fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_http_client_returns_ok() {
        let client = build_http_client(5);
        assert!(client.is_ok(), "client builder should succeed: {:?}", client.err());
    }

    #[test]
    fn percent_to_u8_clamps_and_rounds() {
        assert_eq!(percent_to_u8(-100.0), 0);
        assert_eq!(percent_to_u8(0.0), 0);
        assert_eq!(percent_to_u8(0.4), 0);
        assert_eq!(percent_to_u8(0.5), 1);
        assert_eq!(percent_to_u8(49.6), 50);
        assert_eq!(percent_to_u8(99.4), 99);
        assert_eq!(percent_to_u8(99.5), 100);
        assert_eq!(percent_to_u8(100.0), 100);
        assert_eq!(percent_to_u8(250.0), 100);
        assert_eq!(percent_to_u8(f64::NAN), 0);
    }

    #[test]
    fn home_dir_returns_path_from_env() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let original = std::env::var_os("HOME");
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: serialized via test_fs_lock above so other tests cannot
        // observe the temporary HOME swap; restored before the guard drops.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        let result = home_dir();
        unsafe {
            match original {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let path = result.unwrap();
        assert_eq!(path, dir.path());
    }

    #[test]
    fn home_dir_errors_when_unset() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let original = std::env::var_os("HOME");
        // SAFETY: serialized via test_fs_lock above; restored unconditionally.
        unsafe {
            std::env::remove_var("HOME");
        }
        let result = home_dir();
        unsafe {
            if let Some(value) = original {
                std::env::set_var("HOME", value);
            }
        }
        let err = result.expect_err("home_dir without HOME must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("HOME is not set"), "missing context: {msg}");
    }

    #[test]
    fn send_request_returns_provider_context_on_connection_refused() {
        let client = build_http_client(2).unwrap();
        // 127.0.0.1:1 is virtually always closed.
        let request = client.get("http://127.0.0.1:1/never");
        let result = send_request(request, "test-provider");
        let err = result.expect_err("connection refused expected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("test-provider"),
            "error should include provider context: {msg}"
        );
    }

    fn spawn_one_shot_http_responder(body: &'static str) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        addr
    }

    #[test]
    fn parse_json_response_returns_value_for_valid_json() {
        let addr = spawn_one_shot_http_responder(r#"{"hello":"world"}"#);
        let client = build_http_client(5).unwrap();
        let req = client.get(format!("http://{addr}/"));
        let resp = send_request(req, "json-test").unwrap();
        let value = parse_json_response(resp, "json-test").unwrap();
        assert_eq!(value["hello"].as_str(), Some("world"));
    }

    #[test]
    fn parse_json_response_returns_provider_context_on_malformed_body() {
        let addr = spawn_one_shot_http_responder("not-json-at-all");
        let client = build_http_client(5).unwrap();
        let req = client.get(format!("http://{addr}/"));
        let resp = send_request(req, "malformed-test").unwrap();
        let result = parse_json_response(resp, "malformed-test");
        let err = result.expect_err("malformed JSON must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("malformed-test"),
            "error should include provider context: {msg}"
        );
    }
}
