//! Multi-call shim installed as `wp`, `php`, `composer` and `mysql` in
//! ~/.ferro/bin. Resolves the current directory to a ferro site and
//! execs the right bundled binary with the right environment.

use ferro_core::config::Config;
use ferro_core::paths;
use ferro_core::site::Registry;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

struct Resolved {
    root: PathBuf,
    php_version: String,
    #[allow(dead_code)]
    site_name: Option<String>,
    db: Option<(String, String, String)>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let invoked = Path::new(&args[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let (tool, rest) = match invoked.as_str() {
        "wp" | "php" | "composer" | "mysql" => (invoked, args[1..].to_vec()),
        _ => {
            if args.len() < 2 {
                eprintln!("ferro shim: expected to be called as wp, php, composer or mysql");
                std::process::exit(2);
            }
            (args[1].clone(), args[2..].to_vec())
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let resolved = resolve(&cwd);
    let config = Config::load();

    let code = match tool.as_str() {
        "wp" => run_wp(resolved, &rest),
        "php" => run_php(resolved, &config, &rest),
        "composer" => run_composer(resolved, &config, &rest),
        "mysql" => run_mysql(resolved, &rest),
        other => {
            eprintln!("ferro shim: unknown tool `{other}`");
            2
        }
    };
    std::process::exit(code);
}

/// Resolve a directory to a ferro site: daemon registry first (it knows
/// about live changes), then the registry file with a warning, then a
/// wp-config.php walk-up fallback.
fn resolve(cwd: &Path) -> Option<Resolved> {
    if let Some(site) = ipc_lookup(cwd) {
        return Some(site);
    }
    let from_file = Registry::load();
    if let Some(site) = from_file.find_by_path(cwd) {
        if paths::socket_path().exists() {
            eprintln!("ferro shim: daemon unreachable, using registry file");
        }
        return Some(Resolved {
            root: site.path.clone(),
            php_version: site.php_version.clone(),
            site_name: Some(site.name.clone()),
            db: match (&site.db_name, &site.db_user, &site.db_pass) {
                (Some(n), Some(u), Some(p)) => Some((n.clone(), u.clone(), p.clone())),
                _ => None,
            },
        });
    }
    // Unmanaged WordPress: walk up for wp-config.php inside a parked path.
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join("wp-config.php").exists() {
            return Some(Resolved {
                root: d.to_path_buf(),
                php_version: Config::load().default_php,
                site_name: None,
                db: None,
            });
        }
        dir = d.parent();
    }
    None
}

/// Ask the daemon (short timeout) who owns this path.
fn ipc_lookup(cwd: &Path) -> Option<Resolved> {
    let mut stream = std::os::unix::net::UnixStream::connect(paths::socket_path()).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok()?;
    let request = json!({
        "id": 1,
        "method": "site.lookup",
        "params": {"path": cwd}
    });
    stream.write_all(request.to_string().as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    let response: Value = serde_json::from_str(&line).ok()?;
    let result = response.get("result")?;
    let site = result.get("site")?;
    Some(Resolved {
        root: PathBuf::from(site.get("path")?.as_str()?),
        php_version: site.get("php_version")?.as_str()?.to_string(),
        site_name: site.get("name").and_then(|n| n.as_str()).map(String::from),
        db: match (
            site.get("db_name"),
            site.get("db_user"),
            site.get("db_pass"),
        ) {
            (Some(n), Some(u), Some(p)) => Some((
                n.as_str()?.to_string(),
                u.as_str()?.to_string(),
                p.as_str()?.to_string(),
            )),
            _ => None,
        },
    })
}

fn php_or_die(version: &str) -> PathBuf {
    let php = ferro_core::wpcli::php_binary(version);
    if !php.exists() {
        eprintln!(
            "ferro: PHP {version} is not installed ({})\n  fix: run scripts/fetch-php.sh {version}",
            php.display()
        );
        std::process::exit(1);
    }
    php
}

fn run_wp(resolved: Option<Resolved>, args: &[String]) -> i32 {
    let Some(site) = resolved else {
        // Outside any site: delegate to a real wp further down PATH.
        if let Some(real) = find_on_path("wp") {
            let err = Command::new(real).args(args).exec();
            eprintln!("ferro shim: could not exec delegated wp: {err}");
            return 127;
        }
        let sites = Registry::load();
        eprintln!("ferro: not inside a known site folder.");
        if !sites.sites.is_empty() {
            eprintln!("known sites:");
            for site in &sites.sites {
                eprintln!("  {:<20} {}", site.name, site.path.display());
            }
        }
        eprintln!("or run `ferro wp <site> -- <args>` from anywhere.");
        return 1;
    };
    let phar = ferro_core::wpcli::wp_cli_phar();
    if !phar.exists() {
        eprintln!(
            "ferro: wp-cli.phar missing at {}\n  fix: run scripts/fetch-wp-cli.sh",
            phar.display()
        );
        return 1;
    }
    let php = php_or_die(&site.php_version);
    let mut command = Command::new(&php);
    command
        .arg("-d")
        .arg("memory_limit=1G")
        .args(ferro_core::wpcli::socket_ini_flags())
        .arg(&phar)
        .arg(format!("--path={}", site.root.display()))
        .args(args)
        .env("WP_CLI_PHP", &php)
        .env(
            "WP_CLI_CACHE_DIR",
            paths::app_data_dir().join("cache/wp-cli"),
        );
    let err = command.exec();
    eprintln!("ferro shim: could not exec {}: {err}", php.display());
    127
}

fn run_php(resolved: Option<Resolved>, config: &Config, args: &[String]) -> i32 {
    let version = resolved
        .as_ref()
        .map(|s| s.php_version.as_str())
        .unwrap_or(&config.default_php);
    let php = php_or_die(version);
    let mut command = Command::new(&php);
    command
        .args(ferro_core::wpcli::socket_ini_flags())
        .args(args);
    let err = command.exec();
    eprintln!("ferro shim: could not exec {}: {err}", php.display());
    127
}

fn run_composer(resolved: Option<Resolved>, config: &Config, args: &[String]) -> i32 {
    let phar = ferro_core::wpcli::composer_phar();
    if !phar.exists() {
        eprintln!(
            "ferro: composer.phar missing at {}\n  fix: run scripts/fetch-composer.sh",
            phar.display()
        );
        return 1;
    }
    let version = resolved
        .as_ref()
        .map(|s| s.php_version.as_str())
        .unwrap_or(&config.default_php);
    let php = php_or_die(version);
    let mut command = Command::new(&php);
    command
        .arg("-d")
        .arg("memory_limit=1G")
        .args(ferro_core::wpcli::socket_ini_flags())
        .arg(&phar)
        .args(args)
        .env(
            "COMPOSER_HOME",
            paths::app_data_dir().join("cache/composer"),
        );
    let err = command.exec();
    eprintln!("ferro shim: could not exec composer: {err}");
    127
}

fn run_mysql(resolved: Option<Resolved>, args: &[String]) -> i32 {
    let client = ferro_core::wpcli::mysql_client();
    if !client.exists() {
        eprintln!(
            "ferro: mysql client missing at {}\n  fix: run scripts/fetch-mysql.sh",
            client.display()
        );
        return 1;
    }
    let has = |flag: &str, short: &str| {
        args.iter().any(|a| {
            a == flag
                || a.starts_with(&format!("{flag}="))
                || (!short.is_empty() && a.starts_with(short) && a.len() > short.len())
                || a == short
        })
    };
    let mut command = Command::new(&client);
    // --no-defaults is only valid as the very first option.
    let no_defaults = args.iter().any(|a| a == "--no-defaults");
    if no_defaults {
        command.arg("--no-defaults");
    }
    if !has("--socket", "-S") && !has("--host", "-h") {
        command.arg(format!(
            "--socket={}",
            ferro_core::wpcli::mysql_socket().display()
        ));
    }
    if !has("--user", "-u") && !has("--password", "-p") {
        match resolved.and_then(|s| s.db) {
            Some((db, user, pass)) => {
                command
                    .arg(format!("-u{user}"))
                    .arg(format!("-p{pass}"))
                    .arg(db);
            }
            None => {
                command.arg("-uroot");
            }
        }
    }
    command.args(args.iter().filter(|a| *a != "--no-defaults"));
    let err = command.exec();
    eprintln!("ferro shim: could not exec mysql: {err}");
    127
}

/// The next `name` executable on PATH after our own shims dir.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let shims = paths::shims_dir();
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let dir = PathBuf::from(dir);
        if dir == shims {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
