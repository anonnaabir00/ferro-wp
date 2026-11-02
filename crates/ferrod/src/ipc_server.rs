use crate::state::DaemonState;
use ferro_core::ipc::{Request, Response};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tracing::{debug, warn};

/// Serve JSON-RPC (one request per line) on the ferrod Unix socket.
pub async fn run(
    state: Arc<DaemonState>,
    mut shutdown: watch::Receiver<()>,
) -> std::io::Result<()> {
    let path = ferro_core::paths::socket_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    // Only the owning user may talk to the daemon.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        tokio::spawn(async move { handle(state, stream).await });
                    }
                    Err(err) => warn!("ipc accept failed: {err}"),
                }
            }
            _ = shutdown.changed() => {
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
        }
    }
}

async fn handle(state: Arc<DaemonState>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => dispatch(&state, request),
                    Err(err) => {
                        debug!("bad ipc request: {err}");
                        Response::err(0, -32700, format!("parse error: {err}"))
                    }
                };
                let mut out = match serde_json::to_vec(&response) {
                    Ok(v) => v,
                    Err(err) => {
                        warn!("could not encode ipc response: {err}");
                        continue;
                    }
                };
                out.push(b'\n');
                if write.write_all(&out).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(_) => return,
        }
    }
}

fn dispatch(state: &DaemonState, request: Request) -> Response {
    match request.method.as_str() {
        "ping" => Response::ok(
            request.id,
            serde_json::json!({"pong": true, "version": ferro_core::VERSION}),
        ),
        "status" => match serde_json::to_value(state.status()) {
            Ok(v) => Response::ok(request.id, v),
            Err(err) => Response::err(request.id, -32603, format!("status failed: {err}")),
        },
        other => Response::err(
            request.id,
            -32601,
            format!("unknown method `{other}` (Phase 1 supports: ping, status)"),
        ),
    }
}
