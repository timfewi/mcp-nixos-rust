//! Bounded HTTP access to upstream APIs.
//!
//! Every response body is read through one size cap, and upstream failures are
//! reported with the same wording as `requests`' `raise_for_status`.

use std::fmt;
use std::time::Duration;

use reqwest::{Certificate, Client, Method, RequestBuilder, StatusCode};
use serde_json::Value;
use webpki_root_certs::TLS_SERVER_ROOT_CERTS;

use crate::config::{MAX_RESPONSE_BYTES, NIXOS_AUTH, USER_AGENT};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub enum FetchError {
    Timeout,
    Status(u16, String),
    Transport(String),
    Parse(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("Connection timed out"),
            Self::Status(_, msg) | Self::Transport(msg) | Self::Parse(msg) => f.write_str(msg),
        }
    }
}

impl From<reqwest::Error> for FetchError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() { Self::Timeout } else { Self::Transport(err.to_string()) }
    }
}

/// A fully read upstream response.
pub struct Response {
    pub status: StatusCode,
    pub url: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn status(&self) -> u16 {
        self.status.as_u16()
    }

    /// `requests.Response.raise_for_status`, down to the message.
    pub fn ok(self) -> Result<Self, FetchError> {
        let code = self.status.as_u16();
        let kind = match code {
            400..=499 => "Client",
            500..=599 => "Server",
            _ => return Ok(self),
        };
        let reason = self.status.canonical_reason().unwrap_or("");
        Err(FetchError::Status(code, format!("{code} {kind} Error: {reason} for url: {}", self.url)))
    }

    pub fn json(&self) -> Result<Value, FetchError> {
        serde_json::from_slice(&self.body).map_err(|err| FetchError::Parse(err.to_string()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn bundled_roots() -> Vec<Certificate> {
    TLS_SERVER_ROOT_CERTS.iter().filter_map(|root| Certificate::from_der(root).ok()).collect()
}

pub struct Http {
    client: Client,
    /// Test seam: `https://host/path` is requested as `{upstream}/host/path`.
    upstream: Option<String>,
}

impl Http {
    pub fn new(upstream: Option<String>) -> Result<Self, reqwest::Error> {
        // System roots win, so corporate CAs keep working. Hosts without any
        // (minimal containers, build sandboxes) fall back to the bundled Mozilla
        // set instead of failing to start, which would also take down the
        // offline `store` and `flake-inputs` actions.
        let builder = || Client::builder().user_agent(USER_AGENT).connect_timeout(CONNECT_TIMEOUT);
        let client = builder().build().or_else(|_| builder().tls_certs_only(bundled_roots()).build())?;
        Ok(Self { client, upstream })
    }

    /// `MCP_NIXOS_UPSTREAM` redirects all upstream traffic, for offline tests.
    pub fn from_env() -> Result<Self, reqwest::Error> {
        Self::new(std::env::var("MCP_NIXOS_UPSTREAM").ok().filter(|value| !value.is_empty()))
    }

    fn target(&self, url: &str) -> String {
        match &self.upstream {
            Some(base) => format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches("https://")),
            None => url.to_owned(),
        }
    }

    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client.request(method, self.target(url))
    }

    pub fn head(&self, url: &str) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }

    pub fn get(&self, url: &str) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Authenticated JSON POST against the NixOS search backend.
    pub fn post_es(&self, url: &str, body: &Value) -> RequestBuilder {
        self.request(Method::POST, url).basic_auth(NIXOS_AUTH.0, Some(NIXOS_AUTH.1)).json(body)
    }

    pub async fn send(&self, builder: RequestBuilder, timeout_secs: u64) -> Result<Response, FetchError> {
        self.send_capped(builder, timeout_secs, MAX_RESPONSE_BYTES)
            .await?
            .ok_or_else(|| FetchError::Transport(format!("Response larger than {MAX_RESPONSE_BYTES} bytes")))
    }

    /// Like [`Http::send`], but `None` means the body exceeded `cap`.
    async fn send_capped(
        &self,
        builder: RequestBuilder,
        timeout_secs: u64,
        cap: usize,
    ) -> Result<Option<Response>, FetchError> {
        let response = read_body(builder, timeout_secs, cap).await?;
        Ok((response.body.len() <= cap).then_some(response))
    }

    /// Read at most `cap + 1` bytes, so the caller can tell a body of exactly
    /// `cap` bytes from a longer one without downloading all of it.
    pub async fn send_truncated(
        &self,
        builder: RequestBuilder,
        timeout_secs: u64,
        cap: usize,
    ) -> Result<Response, FetchError> {
        read_body(builder, timeout_secs, cap).await
    }
}

async fn read_body(builder: RequestBuilder, timeout_secs: u64, cap: usize) -> Result<Response, FetchError> {
    let mut response = builder.timeout(Duration::from_secs(timeout_secs)).send().await?;
    let status = response.status();
    let url = response.url().to_string();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > cap {
            break;
        }
    }
    Ok(Response { status, url, body })
}
