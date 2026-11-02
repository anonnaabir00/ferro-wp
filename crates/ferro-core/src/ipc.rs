use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, code: i64, message: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error(
        "cannot connect to ferrod at {} — is the daemon running? try `ferro daemon start`",
        crate::paths::socket_path().display()
    )]
    Connect {
        #[source]
        source: std::io::Error,
    },
    #[error("connection to ferrod closed without a reply")]
    Closed,
    #[error("I/O error talking to ferrod: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid reply from ferrod: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// One request/response round-trip against the daemon.
pub async fn call(method: &str, params: Value) -> Result<Value, IpcError> {
    let stream = UnixStream::connect(crate::paths::socket_path())
        .await
        .map_err(|source| IpcError::Connect { source })?;
    let (read, mut write) = stream.into_split();
    let request = Request {
        id: 1,
        method: method.to_string(),
        params,
    };
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    write.write_all(&line).await?;

    let mut lines = BufReader::new(read).lines();
    let reply = lines.next_line().await?.ok_or(IpcError::Closed)?;
    let response: Response = serde_json::from_str(&reply)?;
    match (response.result, response.error) {
        (_, Some(err)) => Err(IpcError::Rpc(err)),
        (Some(result), _) => Ok(result),
        _ => Err(IpcError::Closed),
    }
}

/// A port whose owner is blocking a ferro service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortConflict {
    pub port: u16,
    pub pid: u32,
    pub command: String,
    pub fix: String,
}

/// Live snapshot reported by the `status` IPC method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    pub http_port: u16,
    pub https_port: u16,
    pub mysql_port: u16,
    pub router_running: bool,
    pub conflicts: Vec<PortConflict>,
    pub sites: Vec<String>,
}
