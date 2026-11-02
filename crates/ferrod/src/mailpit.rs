//! Mailpit — the local mail catcher. Supervised like MySQL/php-fpm:
//! SMTP on 127.0.0.1:1025, web UI on 127.0.0.1:8025 (served by the
//! router at mail.localhost). Optional: if the binary is not installed
//! yet (scripts/fetch-mailpit.sh), the daemon simply skips it.

use crate::supervisor::{ServiceSpec, Supervisor};
use tracing::info;

pub const SMTP_PORT: u16 = 1025;
pub const UI_PORT: u16 = 8025;

pub fn ensure_running(supervisor: &Supervisor) {
    let binary = ferro_core::paths::bin_dir().join("mailpit");
    if !binary.exists() {
        info!("mailpit not installed — skipping (fetch with `ferro update mailpit`)");
        return;
    }
    supervisor.start(ServiceSpec {
        name: "mailpit".into(),
        program: binary,
        args: vec![
            "--listen".into(),
            format!("127.0.0.1:{UI_PORT}"),
            "--smtp".into(),
            format!("127.0.0.1:{SMTP_PORT}"),
        ],
        env: vec![],
    });
}
