use crate::supervisor::{ServiceSpec, Supervisor};
use ferro_core::paths;
use ferro_core::site::{Registry, Site};
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use tracing::info;

fn fpm_binary(version: &str) -> PathBuf {
    paths::bin_dir().join("php").join(version).join("php-fpm")
}

fn fpm_config(version: &str) -> PathBuf {
    paths::sites_dir().join(format!("fpm-{version}.conf"))
}

pub fn fpm_service_name(version: &str) -> String {
    format!("php-fpm-{version}")
}

pub fn available_versions() -> Vec<String> {
    let dir = paths::bin_dir().join("php");
    let mut versions: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("php-fpm").exists())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    versions.sort();
    versions
}

/// PHP versions needed by running sites.
pub fn versions_in_use(registry: &Registry) -> BTreeSet<String> {
    registry
        .sites
        .iter()
        .filter(|s| s.running)
        .map(|s| s.php_version.clone())
        .collect()
}

/// Render the fpm master config for one version: one pool per site.
/// Idempotent — regenerate from the registry every time.
fn render_config(version: &str, sites: &[&Site]) -> String {
    let mut out = format!(
        "[global]\n\
         pid = {}/fpm-{version}.pid\n\
         error_log = {}/php-{version}-fpm.log\n\
         daemonize = no\n\
         log_level = warning\n\n",
        paths::sites_dir().display(),
        paths::logs_dir().display(),
    );
    for site in sites {
        let runtime = paths::sites_dir().join(&site.name);
        let _ = std::fs::create_dir_all(&runtime);
        out.push_str(&format!(
            "[{name}]\n\
             listen = {socket}\n\
             listen.mode = 0666\n\
             pm = ondemand\n\
             pm.max_children = 8\n\
             pm.process_idle_timeout = 10s\n\
             chdir = {root}\n\
             catch_workers_output = yes\n\
             php_admin_value[error_log] = {log}\n\
             php_admin_flag[log_errors] = on\n\
             php_admin_value[memory_limit] = 512M\n\n",
            name = site.name,
            socket = paths::pool_socket(&site.name).display(),
            root = site.path.display(),
            log = paths::logs_dir()
                .join(format!("{}-php.log", site.name))
                .display(),
        ));
    }
    out
}

/// Write config files for all versions in use, only when the content
/// actually changed (regeneration is idempotent). Returns the versions
/// whose config changed.
pub fn write_configs(registry: &Registry) -> io::Result<Vec<String>> {
    let mut changed = Vec::new();
    for version in versions_in_use(registry) {
        let sites: Vec<&Site> = registry
            .sites
            .iter()
            .filter(|s| s.running && s.php_version == version)
            .collect();
        let rendered = render_config(&version, &sites);
        let path = fpm_config(&version);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing != rendered {
            std::fs::write(&path, rendered)?;
            changed.push(version);
        }
    }
    Ok(changed)
}

/// Start one fpm master per PHP version in use; restart masters whose
/// config changed; stop masters that no longer have any sites.
/// (Restart instead of SIGUSR2 reload: a reload that coincides with the
/// runtime dir being removed can leave pools bound to unlinked sockets.)
pub fn reconcile(supervisor: &Supervisor, registry: &Registry) {
    let changed = match write_configs(registry) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!("could not write php-fpm configs: {err}");
            return;
        }
    };
    let wanted = versions_in_use(registry);
    // Stop masters for versions no longer used.
    for service in supervisor.list() {
        if let Some(version) = service.name.strip_prefix("php-fpm-") {
            if !wanted.contains(version) {
                supervisor.stop(&service.name);
            }
        }
    }
    for version in wanted {
        let binary = fpm_binary(&version);
        if !binary.exists() {
            tracing::error!(
                "PHP {version} is not installed ({}); run scripts/fetch-php.sh {version}",
                binary.display()
            );
            continue;
        }
        let service = fpm_service_name(&version);
        if supervisor.is_running(&service) {
            if changed.contains(&version) {
                info!(version, "restarting php-fpm (config changed)");
                supervisor.restart(&service);
            }
        } else if !supervisor.list().iter().any(|s| s.name == service) {
            supervisor.start(ServiceSpec {
                name: service.clone(),
                program: binary,
                args: vec!["-y".into(), fpm_config(&version).display().to_string()],
                env: vec![],
            });
        } else if changed.contains(&version) {
            // Stopped mid-restart; make sure it comes back with new config.
            supervisor.restart(&service);
        }
    }
}
