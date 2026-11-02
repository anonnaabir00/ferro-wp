use crate::supervisor::Supervisor;
use ferro_core::config::Config;
use ferro_core::ipc::{PortConflict, SiteSummary, StatusReport};
use ferro_core::site::{Registry, Site};
use ferro_core::Event;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// One-time, short-lived admin login token for the mu-plugin flow.
pub struct LoginToken {
    pub site: String,
    pub user: Option<String>,
    pub expires: Instant,
}

pub struct DaemonState {
    pub started: Instant,
    pub config: RwLock<Config>,
    pub registry: Arc<RwLock<Registry>>,
    pub supervisor: Supervisor,
    pub events: broadcast::Sender<Event>,
    pub router_running: AtomicBool,
    pub https_running: AtomicBool,
    pub login_tokens: Mutex<HashMap<String, LoginToken>>,
}

impl DaemonState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        registry: Arc<RwLock<Registry>>,
        supervisor: Supervisor,
        events: broadcast::Sender<Event>,
        router_running: bool,
        https_running: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            config: RwLock::new(config),
            registry,
            supervisor,
            events,
            router_running: AtomicBool::new(router_running),
            https_running: AtomicBool::new(https_running),
            login_tokens: Mutex::new(HashMap::new()),
        })
    }

    /// Mint a one-time login token (60 s) for a site's admin area.
    pub fn create_login_token(&self, site: &str, user: Option<String>) -> String {
        let token = ferro_core::random_token(32);
        self.login_tokens.lock().unwrap().insert(
            token.clone(),
            LoginToken {
                site: site.to_string(),
                user,
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
        token
    }

    /// Consume a login token: single-use and expired tokens are rejected.
    pub fn consume_login_token(&self, token: &str) -> Option<(String, Option<String>)> {
        let mut tokens = self.login_tokens.lock().unwrap();
        tokens.retain(|_, t| t.expires > Instant::now());
        let token = tokens.remove(token)?;
        Some((token.site, token.user))
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    pub fn managed_ports(&self) -> [u16; 3] {
        let config = self.config();
        [config.http_port, config.https_port, config.mysql_port]
    }

    /// Live conflict check, excluding ferrod itself.
    pub fn conflicts(&self) -> Vec<PortConflict> {
        let mut conflicts = Vec::new();
        for port in self.managed_ports() {
            if let Some(owner) = ferro_core::ports::owner_of(port) {
                if !owner.is_ferro() {
                    conflicts.push(PortConflict {
                        port,
                        pid: owner.pid,
                        command: owner.full_command.clone(),
                        fix: ferro_core::ports::suggest_fix(&owner, port),
                    });
                }
            }
        }
        conflicts
    }

    pub fn status(&self) -> StatusReport {
        let config = self.config();
        let registry = self.registry.read().unwrap();
        StatusReport {
            version: ferro_core::VERSION.to_string(),
            pid: std::process::id(),
            uptime_secs: self.started.elapsed().as_secs(),
            http_port: config.http_port,
            https_port: config.https_port,
            mysql_port: config.mysql_port,
            router_running: self.router_running.load(Ordering::Relaxed),
            https_running: self.https_running.load(Ordering::Relaxed),
            conflicts: self.conflicts(),
            services: self.supervisor.list(),
            sites: registry
                .sites
                .iter()
                .map(|s| SiteSummary {
                    name: s.name.clone(),
                    php_version: s.php_version.clone(),
                    running: s.running,
                    url: s.url(config.https_port),
                })
                .collect(),
        }
    }

    pub fn registry_snapshot(&self) -> Vec<Site> {
        self.registry.read().unwrap().sites.clone()
    }

    /// Persist the registry, regenerate php-fpm configs, reconcile services.
    pub fn after_registry_change(&self) -> Result<(), String> {
        {
            let registry = self.registry.read().unwrap();
            registry
                .save()
                .map_err(|e| format!("could not save sites.json: {e}"))?;
        }
        let registry = self.registry.read().unwrap().clone();
        crate::php::reconcile(&self.supervisor, &registry);
        Ok(())
    }

    pub fn emit(&self, kind: &str, message: impl Into<String>) {
        let _ = self.events.send(Event::new(kind, message));
    }
}
