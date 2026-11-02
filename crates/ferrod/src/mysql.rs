use crate::supervisor::{ServiceSpec, Supervisor};
use anyhow::{bail, Context, Result};
use ferro_core::paths;
use ferro_core::site::Site;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use tracing::{info, warn};

fn mysql_dir() -> PathBuf {
    paths::app_data_dir().join("mysql")
}

fn data_dir() -> PathBuf {
    mysql_dir().join("data")
}

pub fn socket_path() -> PathBuf {
    mysql_dir().join("mysql.sock")
}

fn mysqld_binary() -> PathBuf {
    paths::bin_dir().join("mysql/bin/mysqld")
}

fn mysql_client() -> PathBuf {
    paths::bin_dir().join("mysql/bin/mysql")
}

/// Initialize the datadir on first run, then supervise mysqld.
pub fn ensure_running(supervisor: &Supervisor, port: u16) -> Result<()> {
    let mysqld = mysqld_binary();
    if !mysqld.exists() {
        bail!(
            "MySQL is not installed at {}\n  fix: run scripts/fetch-mysql.sh",
            mysqld.display()
        );
    }
    std::fs::create_dir_all(mysql_dir())?;
    if !data_dir().exists() {
        info!("initializing MySQL datadir");
        let output = Command::new(&mysqld)
            .args([
                "--initialize-insecure",
                &format!("--basedir={}", paths::bin_dir().join("mysql").display()),
                &format!("--datadir={}", data_dir().display()),
            ])
            .output()
            .context("failed to run mysqld --initialize-insecure")?;
        if !output.status.success() {
            bail!(
                "mysqld --initialize-insecure failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    supervisor.start(ServiceSpec {
        name: "mysql".into(),
        program: mysqld,
        args: vec![
            format!("--basedir={}", paths::bin_dir().join("mysql").display()),
            format!("--datadir={}", data_dir().display()),
            format!("--socket={}", socket_path().display()),
            format!("--port={port}"),
            "--mysqlx=OFF".into(),
            "--bind-address=127.0.0.1".into(),
            "--skip-name-resolve".into(),
            format!(
                "--log-error={}",
                paths::logs_dir().join("mysql.log").display()
            ),
            format!("--pid-file={}", mysql_dir().join("mysql.pid").display()),
        ],
        env: vec![],
    });
    Ok(())
}

/// Wait until mysqld accepts queries on the socket (max 30 s).
pub fn wait_ready() -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if run_client(&["-e", "SELECT 1"]).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

pub fn is_ready() -> bool {
    run_client(&["-e", "SELECT 1"]).is_ok()
}

/// Run the bundled mysql client as root over the local socket.
fn run_client(args: &[&str]) -> Result<String> {
    let output = Command::new(mysql_client())
        .arg(format!("--socket={}", socket_path().display()))
        .arg("-uroot")
        .args(args)
        .output()
        .context("failed to run mysql client")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_sql(sql: &str) -> Result<()> {
    run_client(&["-e", sql]).map(|_| ())
}

/// Create the site's database and dedicated user (idempotent).
/// The user is created for socket ('localhost') and loopback TCP
/// ('127.0.0.1', '::1') connections — Adminer and GUI clients use TCP.
pub fn ensure_site_db(site: &Site) -> Result<()> {
    let (Some(db), Some(user), Some(pass)) = (&site.db_name, &site.db_user, &site.db_pass) else {
        return Ok(());
    };
    let mut sql = format!(
        "CREATE DATABASE IF NOT EXISTS `{db}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
    );
    for host in ["localhost", "127.0.0.1", "::1"] {
        sql.push_str(&format!(
            "CREATE USER IF NOT EXISTS '{user}'@'{host}' IDENTIFIED BY '{pass}';\
             ALTER USER '{user}'@'{host}' IDENTIFIED BY '{pass}';\
             GRANT ALL PRIVILEGES ON `{db}`.* TO '{user}'@'{host}';"
        ));
    }
    sql.push_str("FLUSH PRIVILEGES;");
    run_sql(&sql)
        .with_context(|| format!("could not create database/user for site {}", site.name))?;
    info!(site = %site.name, db, "database ready");
    Ok(())
}

/// Drop the site's database and user (used by `ferro destroy`).
pub fn drop_site_db(site: &Site) -> Result<()> {
    if let Some(db) = &site.db_name {
        run_sql(&format!("DROP DATABASE IF EXISTS `{db}`;"))
            .with_context(|| format!("could not drop database {db}"))?;
    }
    if let Some(user) = &site.db_user {
        for host in ["localhost", "127.0.0.1", "::1"] {
            run_sql(&format!("DROP USER IF EXISTS '{user}'@'{host}';"))
                .with_context(|| format!("could not drop MySQL user {user}@{host}"))?;
        }
    }
    Ok(())
}

/// Best-effort cleanup if MySQL is mid-startup; never fatal.
pub fn ensure_site_db_best_effort(site: &Site) {
    if let Err(err) = ensure_site_db(site) {
        warn!(site = %site.name, "db setup deferred: {err:#}");
    }
}
