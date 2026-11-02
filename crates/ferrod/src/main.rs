mod ipc_server;
mod mailpit;
mod mysql;
mod php;
mod state;
mod supervisor;

use ferro_core::config::Config;
use ferro_core::site::Registry;
use ferro_router::{tls::Ca, Router, RouterState};
use state::DaemonState;
use std::sync::{Arc, RwLock};
use supervisor::Supervisor;
use tokio::sync::{broadcast, watch};
use tracing::{error, info, warn};
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
    let mut https_blocked = false;
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
                if port == config.https_port {
                    https_blocked = true;
                }
            }
        }
    }

    let (events, _) = broadcast::channel(256);
    let supervisor = Supervisor::new(events.clone());

    // Shared MySQL.
    if let Err(err) = mysql::ensure_running(&supervisor, config.mysql_port) {
        error!("MySQL could not be started: {err:#}");
    } else {
        let events = events.clone();
        tokio::spawn(async move {
            if mysql::wait_ready() {
                info!("MySQL is ready");
                let _ = events.send(ferro_core::Event::new("service.ready", "MySQL ready"));
            } else {
                warn!("MySQL did not become ready within 30 s");
            }
        });
    }

    // Mailpit (local mail catcher), if installed.
    mailpit::ensure_running(&supervisor);

    // Site registry + parked folder discovery.
    let registry = Arc::new(RwLock::new(Registry::load()));
    {
        let mut reg = registry.write().unwrap();
        let added = reg.discover(&config.parked_paths, &config.default_php);
        if !added.is_empty() {
            info!("discovered parked sites: {}", added.join(", "));
        }
        // Built-in Adminer site (database UI): served like any site but
        // from the ferro app data dir, never from a WordPress folder.
        let adminer_dir = ferro_core::paths::adminer_dir();
        if adminer_dir.join("index.php").exists() && reg.get("adminer").is_none() {
            reg.upsert(ferro_core::site::Site {
                name: "adminer".to_string(),
                path: adminer_dir,
                php_version: config.default_php.clone(),
                kind: ferro_core::site::SiteKind::Builtin,
                running: true,
                xdebug: false,
                db_name: None,
                db_user: None,
                db_pass: None,
                media_proxy: None,
                admin_user: None,
            });
            info!(
                "adminer available at https://adminer.localhost:{}",
                config.https_port
            );
        }
        if let Err(err) = reg.save() {
            warn!("could not save sites.json: {err}");
        }
    }

    // Local CA for per-site TLS certs.
    let ca = match Ca::load_or_create(&ferro_core::paths::ca_dir()) {
        Ok(ca) => Arc::new(ca),
        Err(err) => {
            error!("could not set up local CA: {err:#}");
            std::process::exit(1);
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(());

    // Router: HTTP (redirect to HTTPS) + HTTPS (TLS termination, FastCGI).
    let router_state = Arc::new(RouterState {
        registry: registry.clone(),
        config: Arc::new(RwLock::new(config.clone())),
        ca: ca.clone(),
    });
    let router = Router::new(router_state);
    {
        let router = router.clone();
        let rx = shutdown_rx.clone();
        let blocked = http_blocked;
        tokio::spawn(async move {
            if blocked {
                error!("http listener not started: port conflict (run `ferro doctor`)");
                return;
            }
            if let Err(err) = router.run_http(rx).await {
                error!("http listener failed: {err}");
            }
        });
    }
    {
        let router = router.clone();
        let rx = shutdown_rx.clone();
        let blocked = https_blocked;
        tokio::spawn(async move {
            if blocked {
                error!("https listener not started: port conflict (run `ferro doctor`)");
                return;
            }
            if let Err(err) = router.run_https(rx).await {
                error!("https listener failed: {err}");
            }
        });
    }

    let state = DaemonState::new(
        config,
        registry.clone(),
        supervisor.clone(),
        events,
        !http_blocked,
        !https_blocked,
    );

    // php-fpm masters for the versions in use.
    {
        let registry = registry.read().unwrap().clone();
        php::reconcile(&supervisor, &registry);
    }

    let ipc_state = state.clone();
    let ipc_rx = shutdown_rx.clone();
    let ipc = tokio::spawn(async move { ipc_server::run(ipc_state, ipc_rx).await });

    wait_for_shutdown().await;
    info!("ferrod shutting down");
    let _ = shutdown_tx.send(());

    // Graceful shutdown: stop MySQL cleanly so InnoDB does not need
    // recovery on next start.
    for service in supervisor.list() {
        supervisor.stop(&service.name);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if supervisor.list().iter().all(|s| s.pid.is_none()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
