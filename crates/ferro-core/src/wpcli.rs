//! Shared helpers for running site-aware PHP tooling (wp-cli, composer,
//! mysql) with the correct binary, socket and environment.
use crate::paths;
use std::path::PathBuf;
use std::process::Command;

pub fn php_binary(version: &str) -> PathBuf {
    paths::bin_dir().join("php").join(version).join("php")
}

pub fn wp_cli_phar() -> PathBuf {
    // WP-CLI re-launches itself for some subcommands (e.g. `rewrite
    // structure`) without shell-escaping argv[0], and the app-data path
    // contains a space ("Application Support") — the subprocess then
    // fails with "Could not open input file". Prefer the space-free
    // copy installed next to the shims.
    let flat = paths::shims_dir().join("wp-cli.phar");
    if flat.exists() {
        return flat;
    }
    paths::bin_dir().join("wp-cli.phar")
}

pub fn composer_phar() -> PathBuf {
    let flat = paths::shims_dir().join("composer.phar");
    if flat.exists() {
        return flat;
    }
    paths::bin_dir().join("composer.phar")
}

pub fn mysql_client() -> PathBuf {
    paths::bin_dir().join("mysql/bin/mysql")
}

pub fn mysql_socket() -> PathBuf {
    paths::app_data_dir().join("mysql/mysql.sock")
}

/// php -d flags that make DB_HOST=localhost hit ferro's MySQL socket.
pub fn socket_ini_flags() -> Vec<String> {
    let socket = mysql_socket();
    vec![
        "-d".into(),
        format!("mysqli.default_socket={}", socket.display()),
        "-d".into(),
        format!("pdo_mysql.default_socket={}", socket.display()),
    ]
}

/// Build the WP-CLI command for a site: correct PHP, 1G memory limit,
/// MySQL socket ini flags, cache dir under app data.
pub fn wp_command(php_version: &str, site_root: &std::path::Path, args: &[String]) -> Command {
    let php = php_binary(php_version);
    let mut command = Command::new(php.clone());
    command
        .arg("-d")
        .arg("memory_limit=1G")
        .args(socket_ini_flags())
        .arg(wp_cli_phar())
        .arg(format!("--path={}", site_root.display()))
        .args(args)
        .env("WP_CLI_PHP", &php)
        .env(
            "WP_CLI_CACHE_DIR",
            paths::app_data_dir().join("cache/wp-cli"),
        );
    command
}

/// Build the composer command for a site.
pub fn composer_command(php_version: &str, args: &[String]) -> Command {
    let php = php_binary(php_version);
    let mut command = Command::new(php.clone());
    command
        .arg("-d")
        .arg("memory_limit=1G")
        .args(socket_ini_flags())
        .arg(composer_phar())
        .args(args)
        .env(
            "COMPOSER_HOME",
            paths::app_data_dir().join("cache/composer"),
        )
        .env("COMPOSER_ALLOW_SUPERUSER", "1");
    command
}
