use std::io;
use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME environment variable is not set"))
}

/// ~/Library/Application Support/ferro
pub fn app_data_dir() -> PathBuf {
    home_dir().join("Library/Application Support/ferro")
}

/// ~/Library/Logs/ferro
pub fn logs_dir() -> PathBuf {
    home_dir().join("Library/Logs/ferro")
}

/// ~/.ferro/bin — shims and installed binaries on PATH.
pub fn shims_dir() -> PathBuf {
    home_dir().join(".ferro/bin")
}

pub fn socket_path() -> PathBuf {
    app_data_dir().join("ferrod.sock")
}

pub fn bin_dir() -> PathBuf {
    app_data_dir().join("bin")
}

pub fn ca_dir() -> PathBuf {
    app_data_dir().join("ca")
}

pub fn sites_dir() -> PathBuf {
    app_data_dir().join("sites")
}

/// Per-site php-fpm pool socket (the router talks FastCGI to this).
pub fn pool_socket(site_name: &str) -> PathBuf {
    sites_dir().join(site_name).join("php-fpm.sock")
}

pub fn config_path() -> PathBuf {
    app_data_dir().join("config.toml")
}

pub fn launch_agent_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents/dev.ferro.daemon.plist")
}

pub fn ensure_dirs() -> io::Result<()> {
    for dir in [app_data_dir(), logs_dir(), bin_dir(), ca_dir(), sites_dir()] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}
