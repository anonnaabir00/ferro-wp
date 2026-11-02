use crate::state::DaemonState;
use ferro_core::ipc::{Request, Response};
use ferro_core::site::{Site, SiteKind};
use serde_json::{json, Value};
use std::path::PathBuf;
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
                let request = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => request,
                    Err(err) => {
                        debug!("bad ipc request: {err}");
                        write_json(
                            &mut write,
                            &Response::err(0, -32700, format!("parse error: {err}")),
                        )
                        .await;
                        continue;
                    }
                };
                if request.method == "events" {
                    stream_events(&state, &mut write, request.id).await;
                    return;
                }
                let response = dispatch(&state, request);
                write_json(&mut write, &response).await;
            }
            Ok(None) | Err(_) => return,
        }
    }
}

async fn write_json(write: &mut tokio::net::unix::OwnedWriteHalf, value: &impl serde::Serialize) {
    if let Ok(mut out) = serde_json::to_vec(value) {
        out.push(b'\n');
        let _ = write.write_all(&out).await;
    }
}

/// Long-lived subscription: one JSON event per line until disconnect.
async fn stream_events(state: &DaemonState, write: &mut tokio::net::unix::OwnedWriteHalf, id: u64) {
    write_json(write, &Response::ok(id, json!({"subscribed": true}))).await;
    let mut receiver = state.events.subscribe();
    loop {
        match receiver.recv().await {
            Ok(event) => write_json(write, &event).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                write_json(
                    write,
                    &ferro_core::Event::new("events.lagged", format!("dropped {n} events")),
                )
                .await;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn dispatch(state: &Arc<DaemonState>, request: Request) -> Response {
    let result = match request.method.as_str() {
        "ping" => Ok(json!({"pong": true, "version": ferro_core::VERSION})),
        "status" => serde_json::to_value(state.status()).map_err(|e| e.to_string()),
        "site.list" => site_list(state),
        "site.info" => site_info(state, &request.params),
        "site.lookup" => site_lookup(state, &request.params),
        "site.register" => site_register(state, &request.params),
        "site.unregister" => site_unregister(state, &request.params),
        "site.rescan" => site_rescan(state),
        "site.set_running" => site_set_running(state, &request.params),
        "site.set_php" => site_set_php(state, &request.params),
        "service.list" => serde_json::to_value(state.supervisor.list()).map_err(|e| e.to_string()),
        "service.restart" => {
            state.supervisor.restart_all();
            state.emit("service.restart", "all services restarted".to_string());
            Ok(json!({"restarted": true}))
        }
        other => Err(format!("unknown method `{other}`")),
    };
    match result {
        Ok(value) => Response::ok(request.id, value),
        Err(message) => Response::err(request.id, -32000, message),
    }
}

fn param_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing parameter `{key}`"))
}

fn site_list(state: &DaemonState) -> Result<Value, String> {
    serde_json::to_value(state.registry_snapshot()).map_err(|e| e.to_string())
}

fn site_info(state: &DaemonState, params: &Value) -> Result<Value, String> {
    let name = param_str(params, "name")?;
    let registry = state.registry.read().unwrap();
    let site = registry
        .get(name)
        .ok_or_else(|| format!("no site named `{name}` (see `ferro sites`)"))?;
    let config = state.config();
    Ok(json!({
        "site": site,
        "url": site.url(config.https_port),
        "admin_url": format!("{}/wp-admin/", site.url(config.https_port)),
        "db_socket": crate::mysql::socket_path(),
        "mysql_port": config.mysql_port,
        "fpm_socket": ferro_core::paths::pool_socket(&site.name),
        "php_log": ferro_core::paths::logs_dir().join(format!("{name}-php.log")),
    }))
}

fn site_lookup(state: &DaemonState, params: &Value) -> Result<Value, String> {
    let path = param_str(params, "path")?;
    let registry = state.registry.read().unwrap();
    let site = registry
        .find_by_path(std::path::Path::new(path))
        .ok_or_else(|| format!("no registered site contains {path}"))?;
    serde_json::to_value(json!({ "site": site })).map_err(|e| e.to_string())
}

fn site_register(state: &Arc<DaemonState>, params: &Value) -> Result<Value, String> {
    let name = param_str(params, "name")?.to_lowercase();
    Site::validate_name(&name)?;
    let config = state.config();
    let path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) => {
            let path = PathBuf::from(p);
            if !path.is_dir() {
                return Err(format!("{} is not a folder", path.display()));
            }
            path.canonicalize()
                .map_err(|e| format!("could not resolve {}: {e}", path.display()))?
        }
        None => {
            let path = config
                .parked_paths
                .first()
                .cloned()
                .unwrap_or_else(|| ferro_core::paths::home_dir().join("Sites"))
                .join(&name);
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("could not create {}: {e}", path.display()))?;
            path
        }
    };
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some("created") => SiteKind::Created,
        Some("linked") => SiteKind::Linked,
        _ => SiteKind::Parked,
    };
    let php = params
        .get("php")
        .and_then(|v| v.as_str())
        .unwrap_or(&config.default_php)
        .to_string();
    let site = {
        let mut registry = state.registry.write().unwrap();
        let existing = registry.get(&name).cloned();
        let site = Site {
            db_name: Some(Site::db_ident(&name)),
            db_user: Some(Site::db_ident(&name)),
            db_pass: existing
                .as_ref()
                .and_then(|s| s.db_pass.clone())
                .or_else(|| Some(ferro_core::random_token(24))),
            media_proxy: existing.as_ref().and_then(|s| s.media_proxy.clone()),
            name: name.clone(),
            path,
            php_version: php,
            kind,
            running: true,
            xdebug: existing.as_ref().map(|s| s.xdebug).unwrap_or(false),
        };
        registry.upsert(site.clone());
        site
    };
    if crate::mysql::is_ready() {
        crate::mysql::ensure_site_db_best_effort(&site);
    }
    state
        .after_registry_change()
        .map_err(|e| format!("registry update failed: {e}"))?;
    state.emit("site.added", format!("{} registered", site.name));
    serde_json::to_value(&site).map_err(|e| e.to_string())
}

fn site_unregister(state: &Arc<DaemonState>, params: &Value) -> Result<Value, String> {
    let name = param_str(params, "name")?;
    let drop_db = params
        .get("drop_db")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let site = {
        let mut registry = state.registry.write().unwrap();
        registry
            .remove(name)
            .ok_or_else(|| format!("no site named `{name}`"))?
    };
    if drop_db && crate::mysql::is_ready() {
        if let Err(err) = crate::mysql::drop_site_db(&site) {
            warn!("dropping db for {name} failed: {err:#}");
        }
    }
    // Remove per-site runtime dir (fpm socket etc.).
    let _ = std::fs::remove_dir_all(ferro_core::paths::sites_dir().join(name));
    state
        .after_registry_change()
        .map_err(|e| format!("registry update failed: {e}"))?;
    state.emit("site.removed", format!("{name} unregistered"));
    Ok(json!({"removed": name, "db_dropped": drop_db}))
}

fn site_rescan(state: &Arc<DaemonState>) -> Result<Value, String> {
    let added = {
        let config = state.config();
        let mut registry = state.registry.write().unwrap();
        registry.discover(&config.parked_paths, &config.default_php)
    };
    if !added.is_empty() {
        let mysql_ready = crate::mysql::is_ready();
        for name in &added {
            if mysql_ready {
                let site = state.registry.read().unwrap().get(name).cloned();
                if let Some(site) = site {
                    crate::mysql::ensure_site_db_best_effort(&site);
                }
            }
            state.emit("site.added", format!("{name} discovered"));
        }
        state
            .after_registry_change()
            .map_err(|e| format!("registry update failed: {e}"))?;
    }
    Ok(json!({"added": added}))
}

fn site_set_running(state: &Arc<DaemonState>, params: &Value) -> Result<Value, String> {
    let name = param_str(params, "name")?;
    let running = params
        .get("running")
        .and_then(|v| v.as_bool())
        .ok_or("missing parameter `running`")?;
    {
        let mut registry = state.registry.write().unwrap();
        let site = registry
            .sites
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("no site named `{name}`"))?;
        site.running = running;
    }
    state
        .after_registry_change()
        .map_err(|e| format!("registry update failed: {e}"))?;
    state.emit(
        "site.toggled",
        format!("{} {}", name, if running { "started" } else { "stopped" }),
    );
    Ok(json!({"name": name, "running": running}))
}

fn site_set_php(state: &Arc<DaemonState>, params: &Value) -> Result<Value, String> {
    let name = param_str(params, "name")?;
    let version = param_str(params, "version")?;
    let available = crate::php::available_versions();
    if !available.iter().any(|v| v == version) {
        return Err(format!(
            "PHP {version} is not installed (installed: {}). Fetch it with scripts/fetch-php.sh {version}.",
            available.join(", ")
        ));
    }
    {
        let mut registry = state.registry.write().unwrap();
        let site = registry
            .sites
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("no site named `{name}`"))?;
        site.php_version = version.to_string();
    }
    state
        .after_registry_change()
        .map_err(|e| format!("registry update failed: {e}"))?;
    state.emit("site.php", format!("{name} switched to PHP {version}"));
    Ok(json!({"name": name, "php_version": version}))
}
