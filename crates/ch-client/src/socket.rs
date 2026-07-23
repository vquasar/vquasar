//! Minimal HTTP/1 client over the Cloud Hypervisor API Unix socket.
//!
//! Cloud Hypervisor serves a small REST API over a Unix domain socket, with all
//! endpoints under the `/api/v1` prefix. This module speaks just enough HTTP/1
//! (via `hyper`) to drive it, and knows how to wait for a freshly launched VMM
//! to start answering `vmm.ping`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::UnixStream;
use tokio::time::{sleep, Instant};

use crate::error::{ChError, Result};

/// The path prefix every Cloud Hypervisor API endpoint lives under.
const API_BASE: &str = "/api/v1";

/// A client bound to one Cloud Hypervisor API socket.
#[derive(Debug, Clone)]
pub struct ApiClient {
    socket: PathBuf,
}

impl ApiClient {
    /// Bind to the API socket at `socket` (the file need not exist yet).
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// The socket path this client targets.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Send one request and collect the full response.
    ///
    /// A fresh connection is opened per request; the CH API is low-frequency
    /// and this keeps the client simple and free of pooled-connection state.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(StatusCode, Vec<u8>)> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(ChError::Io)?;
        let io = TokioIo::new(stream);

        let (mut sender, conn) = http1::handshake(io)
            .await
            .map_err(|e| ChError::Transport(e.to_string()))?;
        // Drive the connection in the background for the life of the request.
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let uri = format!("{API_BASE}{path}");
        let payload = body.unwrap_or_default();
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(payload)))
            .map_err(|e| ChError::Transport(e.to_string()))?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|e| ChError::Transport(e.to_string()))?;

        let status = response.status();
        let collected = response
            .into_body()
            .collect()
            .await
            .map_err(|e| ChError::Transport(e.to_string()))?;

        Ok((status, collected.to_bytes().to_vec()))
    }

    /// `PUT` a JSON body to an endpoint that returns no content on success
    /// (e.g. `vm.create`, `vm.boot`, `vm.shutdown`, `vm.delete`).
    pub async fn put_json<T: Serialize>(&self, path: &str, body: &T) -> Result<()> {
        let payload = serde_json::to_vec(body).map_err(ChError::Serialization)?;
        let (status, resp) = self.send(Method::PUT, path, Some(payload)).await?;
        ensure_success(status, &resp)
    }

    /// `PUT` with no request body to an endpoint that returns no content.
    pub async fn put_empty(&self, path: &str) -> Result<()> {
        let (status, resp) = self.send(Method::PUT, path, None).await?;
        ensure_success(status, &resp)
    }

    /// `GET` a JSON response.
    pub async fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        let (status, resp) = self.send(Method::GET, path, None).await?;
        ensure_success(status, &resp)?;
        serde_json::from_slice(&resp).map_err(ChError::Serialization)
    }

    /// Whether the VMM answers `vmm.ping`.
    pub async fn ping(&self) -> Result<()> {
        let (status, resp) = self.send(Method::GET, "/vmm.ping", None).await?;
        ensure_success(status, &resp)
    }

    /// Poll `vmm.ping` until it succeeds or `timeout` elapses.
    ///
    /// Used after launching a `cloud-hypervisor` process to wait for its API
    /// socket to come up (design document, section 22: "wait for API socket").
    pub async fn wait_ready(&self, timeout: Duration, poll_interval: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.ping().await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ChError::SocketTimeout);
            }
            sleep(poll_interval).await;
        }
    }
}

/// Map a 2xx status to `Ok(())`, everything else to [`ChError::Api`].
fn ensure_success(status: StatusCode, body: &[u8]) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(ChError::Api {
            status: status.as_u16(),
            body: String::from_utf8_lossy(body).into_owned(),
        })
    }
}
