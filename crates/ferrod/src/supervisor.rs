use ferro_core::ipc::ServiceInfo;
use ferro_core::Event;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

/// How to run one supervised service.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

struct ServiceEntry {
    spec: ServiceSpec,
    status: &'static str,
    pid: Option<u32>,
    restarts: u32,
    detail: Option<String>,
    stop: watch::Sender<bool>,
}

/// Supervises child processes: restarts them on crash with backoff and
/// reports state changes on the event bus.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Mutex<HashMap<String, ServiceEntry>>>,
    events: broadcast::Sender<Event>,
}

impl Supervisor {
    pub fn new(events: broadcast::Sender<Event>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            events,
        }
    }

    pub fn start(&self, spec: ServiceSpec) {
        let name = spec.name.clone();
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(&name) {
            return; // already supervised
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        inner.insert(
            name.clone(),
            ServiceEntry {
                spec: spec.clone(),
                status: "starting",
                pid: None,
                restarts: 0,
                detail: None,
                stop: stop_tx,
            },
        );
        drop(inner);
        let supervisor = self.clone();
        tokio::spawn(async move { supervisor.monitor(spec, stop_rx).await });
    }

    /// Ask a service to stop (SIGTERM, then SIGKILL after a grace period).
    pub fn stop(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.get_mut(name) {
            entry.status = "stopped";
            let _ = entry.stop.send(true);
        }
    }

    #[allow(dead_code)]
    pub fn pid_of(&self, name: &str) -> Option<u32> {
        self.inner.lock().unwrap().get(name).and_then(|e| e.pid)
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(name)
            .map(|e| e.status == "running")
            .unwrap_or(false)
    }

    pub fn list(&self) -> Vec<ServiceInfo> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|e| ServiceInfo {
                name: e.spec.name.clone(),
                status: e.status.to_string(),
                pid: e.pid,
                restarts: e.restarts,
                detail: e.detail.clone(),
            })
            .collect()
    }

    /// Restart one service (stop, then start with its stored spec).
    pub fn restart(&self, name: &str) {
        let spec = {
            let inner = self.inner.lock().unwrap();
            inner.get(name).map(|e| e.spec.clone())
        };
        let Some(spec) = spec else { return };
        self.stop(name);
        let supervisor = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            supervisor.inner.lock().unwrap().remove(&spec.name);
            supervisor.start(spec);
        });
    }

    /// Restart every supervised service.
    pub fn restart_all(&self) {
        let specs: Vec<ServiceSpec> = self
            .inner
            .lock()
            .unwrap()
            .values()
            .map(|e| e.spec.clone())
            .collect();
        for spec in &specs {
            self.stop(&spec.name);
        }
        // Monitors exit on stop; restart after a short grace period.
        let supervisor = self.clone();
        let specs = specs.to_vec();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            for spec in specs {
                // Remove the stopped entry so start() will re-supervise it.
                supervisor.inner.lock().unwrap().remove(&spec.name);
                supervisor.start(spec);
            }
        });
    }

    fn update(&self, name: &str, f: impl FnOnce(&mut ServiceEntry)) {
        if let Some(entry) = self.inner.lock().unwrap().get_mut(name) {
            f(entry);
        }
    }

    async fn monitor(self, spec: ServiceSpec, mut stop_rx: watch::Receiver<bool>) {
        let mut backoff = Duration::from_millis(250);
        loop {
            let started = Instant::now();
            let child = spawn_child(&spec);
            let mut child: Child = match child {
                Ok(child) => {
                    let pid = child.id();
                    info!(service = %spec.name, pid, "service started");
                    self.update(&spec.name, |e| {
                        e.status = "running";
                        e.pid = pid;
                    });
                    let _ = self.events.send(Event::new(
                        "service.started",
                        format!("{} started (PID {})", spec.name, pid.unwrap_or(0)),
                    ));
                    child
                }
                Err(err) => {
                    warn!(service = %spec.name, "failed to spawn: {err:#}");
                    self.update(&spec.name, |e| {
                        e.status = "backoff";
                        e.detail = Some(format!("{err:#}"));
                    });
                    let _ = self.events.send(Event::new(
                        "service.error",
                        format!("{} failed to start: {err:#}", spec.name),
                    ));
                    if wait_or_stop(&mut stop_rx, backoff).await {
                        break;
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                    continue;
                }
            };

            // Wait for exit or stop request.
            let exit_status = tokio::select! {
                status = child.wait() => Some(status),
                _ = stop_rx.changed() => {
                    terminate(&mut child).await;
                    None
                }
            };

            let requested_stop = *stop_rx.borrow();
            if requested_stop {
                self.update(&spec.name, |e| {
                    e.status = "stopped";
                    e.pid = None;
                });
                let _ = self.events.send(Event::new(
                    "service.stopped",
                    format!("{} stopped", spec.name),
                ));
                break;
            }

            let code = exit_status
                .and_then(|s| s.ok())
                .and_then(|s| s.code().map(|c| c.to_string()))
                .unwrap_or_else(|| "signal".to_string());
            warn!(service = %spec.name, code, "service exited");
            let ran_long = started.elapsed() > Duration::from_secs(10);
            self.update(&spec.name, |e| {
                e.status = "backoff";
                e.pid = None;
                e.restarts = if ran_long { 1 } else { e.restarts + 1 };
                e.detail = Some(format!("exited ({code}), restarting"));
            });
            let restarts = self.inner.lock().unwrap()[&spec.name].restarts;
            let _ = self.events.send(Event::new(
                "service.crashed",
                format!(
                    "{} exited ({code}); restarting (attempt {})",
                    spec.name, restarts
                ),
            ));
            if ran_long {
                backoff = Duration::from_millis(250);
            }
            if wait_or_stop(&mut stop_rx, backoff).await {
                break;
            }
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }
}

fn spawn_child(spec: &ServiceSpec) -> std::io::Result<Child> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn()
}

/// SIGTERM, then SIGKILL if it ignores us for 5 s (MySQL needs a clean
/// shutdown so InnoDB does not have to recover on next start).
async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    tokio::select! {
        _ = child.wait() => {}
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

async fn wait_or_stop(stop_rx: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = stop_rx.changed() => true,
    }
}
