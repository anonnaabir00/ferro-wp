mod ipc_server;
mod state;

use ferro_core::config::Config;
use ferro_router::Router;
use state::DaemonState;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    ferro_core::paths::ensure_dirs()?;
    let config = Config::load();
    info!(version = ferro_core::VERSION, "ferrod starting");

    // Port conflict detection happens before we bind anything: never start
    // in a half-working state silently.
    let mut http_blocked = false;
    for port in [config.http_port, config.https_port, config.mysql_port] {
        if let Some(owner) = ferro_core::ports::owner_of(port) {
            if !owner.is_ferro() {
                let fix = ferro_core::ports::suggest_fix(&owner, port);
                error!(
                    port,
                    pid = owner.pid,
                    command = %owner.full_command,
                    "port conflict: {fix}"
                );
                if port == config.http_port {
                    http_blocked = true;
                }
            }
        }
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(());

    if http_blocked {
        error!(
            "router not started: :{} is held by another process (run `ferro doctor`)",
            config.http_port
        );
    } else {
        let router = Arc::new(Router {
            parked_paths: config.parked_paths.clone(),
        });
        let port = config.http_port;
        let rx = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(err) = router.run(port, rx).await {
                error!("router failed on :{port}: {err}");
            }
        });
    }

    let state = DaemonState::new(config, !http_blocked);
    let ipc_state = state.clone();
    let ipc_rx = shutdown_rx.clone();
    let ipc = tokio::spawn(async move { ipc_server::run(ipc_state, ipc_rx).await });

    wait_for_shutdown().await;
    info!("ferrod shutting down");
    let _ = shutdown_tx.send(());
    let _ = ipc.await;
    Ok(())
}

async fn wait_for_shutdown() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}
