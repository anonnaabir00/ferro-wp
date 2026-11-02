use ferro_core::config::Config;
use ferro_core::ipc::{PortConflict, StatusReport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct DaemonState {
    pub started: Instant,
    pub config: Config,
    pub router_running: AtomicBool,
}

impl DaemonState {
    pub fn new(config: Config, router_running: bool) -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            config,
            router_running: AtomicBool::new(router_running),
        })
    }

    /// Ports ferro is responsible for.
    pub fn managed_ports(&self) -> [u16; 3] {
        [
            self.config.http_port,
            self.config.https_port,
            self.config.mysql_port,
        ]
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
        StatusReport {
            version: ferro_core::VERSION.to_string(),
            pid: std::process::id(),
            uptime_secs: self.started.elapsed().as_secs(),
            http_port: self.config.http_port,
            https_port: self.config.https_port,
            mysql_port: self.config.mysql_port,
            router_running: self.router_running.load(Ordering::Relaxed),
            conflicts: self.conflicts(),
            sites: Vec::new(), // site registry lands in Phase 2
        }
    }
}
