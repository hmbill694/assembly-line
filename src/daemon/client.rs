//! The CLI's side of the socket: HTTP/1 over a Unix stream.

use super::api::{Health, Refused};
use super::root::socket_path;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DaemonClient {
    root: PathBuf,
}

#[derive(Debug)]
pub enum ClientError {
    NoDaemon { root: PathBuf },
    Failed(anyhow::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDaemon { root } => write!(
                f,
                "no daemon is listening at {} — start one with `assembly daemon`",
                root.display()
            ),
            Self::Failed(e) => write!(f, "talking to the daemon: {e}"),
        }
    }
}

/// What the daemon answered a request it understood.
#[derive(Debug)]
pub enum Reply<U> {
    Accepted(U),
    Refused(Refused),
}

impl DaemonClient {
    #[must_use]
    pub fn for_root(root: &Path) -> DaemonClient {
        DaemonClient {
            root: root.to_path_buf(),
        }
    }

    /// # Errors
    ///
    /// [`ClientError::NoDaemon`] when nothing listens on the root's socket.
    pub async fn health(&self) -> Result<Health, ClientError> {
        let (_, body) = self.send(Method::GET, "/health", Bytes::new()).await?;
        serde_json::from_slice(&body).map_err(|e| ClientError::Failed(e.into()))
    }

    /// POST `body` as JSON; a `422` is the daemon refusing, with reasons.
    ///
    /// # Errors
    ///
    /// When no daemon listens, or it answers something that is neither.
    pub async fn post_json<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<Reply<U>, ClientError> {
        let json = serde_json::to_vec(body).map_err(|e| ClientError::Failed(e.into()))?;
        let (status, reply) = self.send(Method::POST, path, Bytes::from(json)).await?;
        let answered_otherwise = || {
            ClientError::Failed(anyhow::anyhow!(
                "{status}: {}",
                String::from_utf8_lossy(&reply)
            ))
        };
        match status {
            // A body the daemon could not read is a 422 too, with a plain
            // reason instead of a `Refused` — say it as the daemon did.
            StatusCode::UNPROCESSABLE_ENTITY => serde_json::from_slice(&reply)
                .map(Reply::Refused)
                .map_err(|_| answered_otherwise()),
            status if status.is_success() => serde_json::from_slice(&reply)
                .map(Reply::Accepted)
                .map_err(|e| ClientError::Failed(e.into())),
            _ => Err(answered_otherwise()),
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Bytes,
    ) -> Result<(StatusCode, Bytes), ClientError> {
        let failed = |e: &dyn std::fmt::Display| ClientError::Failed(anyhow::anyhow!("{e}"));
        // A dead daemon's socket refuses connections until the next one
        // clears it, so a refusal means no daemon as surely as a missing file.
        let stream = tokio::net::UnixStream::connect(socket_path(&self.root))
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    ClientError::NoDaemon {
                        root: self.root.clone(),
                    }
                }
                _ => failed(&e),
            })?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| failed(&e))?;
        tokio::spawn(connection);
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "assembly-daemon")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(Full::new(body))
            .map_err(|e| failed(&e))?;
        let response = sender.send_request(request).await.map_err(|e| failed(&e))?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| failed(&e))?
            .to_bytes();
        Ok((status, body))
    }
}
