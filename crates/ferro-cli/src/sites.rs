use anyhow::{bail, Context, Result};
use ferro_core::config::Config;
use ferro_core::ipc::call;
use ferro_core::paths;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

pub async fn park(path: Option<String>, json_mode: bool) -> Result<i32> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home_dir().join("Sites"));
    let path = path
        .canonicalize()
        .with_context(|| format!("{} does not exist", path.display()))?;
    let mut config = Config::load();
    if !config.parked_paths.contains(&path) {
        config.parked_paths.push(path.clone());
        config.save()?;
    }
    let added = call("site.rescan", json!({})).await?;
    let added = added
        .get("added")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if json_mode {
        println!("{}", json!({"parked": path, "added": added}));
    } else {
        println!("parked {}", path.display());
        if added.is_empty() {
            println!("no new sites discovered");
        } else {
            for name in &added {
                println!("  + {}", name.as_str().unwrap_or("?"));
            }
        }
    }
    Ok(0)
}

pub async fn sites(json_mode: bool) -> Result<i32> {
    let value = call("site.list", json!({})).await?;
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }
    let config = Config::load();
    let sites = value.as_array().cloned().unwrap_or_default();
    if sites.is_empty() {
        println!("no sites yet — drop a folder into ~/Sites or run `ferro new <name>`");
        return Ok(0);
    }
    for site in &sites {
        let name = site.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let php = site
            .get("php_version")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let running = site
            .get("running")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let path = site.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let state = if running { "" } else { " (stopped)" };
        println!(
            "  {name:<20} php {php:<4} https://{name}.localhost:{}{state}  {path}",
            config.https_port
        );
    }
    Ok(0)
}

pub async fn link(name: &str, path: Option<String>, json_mode: bool) -> Result<i32> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()
        .context("could not resolve path")?;
    let value = call(
        "site.register",
        json!({"name": name, "path": path, "kind": "linked"}),
    )
    .await?;
    let config = Config::load();
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "linked {} -> https://{}.localhost:{}",
            path.display(),
            name,
            config.https_port
        );
    }
    Ok(0)
}

pub async fn unlink(name: &str, json_mode: bool) -> Result<i32> {
    let value = call("site.unregister", json!({"name": name, "drop_db": false})).await?;
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("unlinked {name} (files and database kept)");
    }
    Ok(0)
}

pub async fn set_running(name: &str, running: bool, json_mode: bool) -> Result<i32> {
    call(
        "site.set_running",
        json!({"name": name, "running": running}),
    )
    .await?;
    if json_mode {
        println!("{}", json!({"name": name, "running": running}));
    } else {
        println!("{} {name}", if running { "started" } else { "stopped" });
    }
    Ok(0)
}

pub async fn restart(name: Option<String>, json_mode: bool) -> Result<i32> {
    match name {
        Some(name) => {
            call("site.set_running", json!({"name": name, "running": false})).await?;
            call("site.set_running", json!({"name": name, "running": true})).await?;
            if json_mode {
                println!("{}", json!({"restarted": name}));
            } else {
                println!("restarted {name}");
            }
        }
        None => {
            call("service.restart", json!({})).await?;
            if json_mode {
                println!("{}", json!({"restarted": "all services"}));
            } else {
                println!("restarted all services (mysql, php-fpm)");
            }
        }
    }
    Ok(0)
}

pub async fn open(name: &str, admin: bool, json_mode: bool) -> Result<i32> {
    let config = Config::load();
    let url = if admin {
        // One-click login: single-use token, consumed by the mu-plugin.
        let value = call("login.create", json!({"name": name})).await?;
        let token = value
            .get("token")
            .and_then(|v| v.as_str())
            .context("daemon did not return a login token")?;
        format!(
            "https://{name}.localhost:{}/?ferro-login={token}",
            config.https_port
        )
    } else {
        format!("https://{name}.localhost:{}/", config.https_port)
    };
    if json_mode {
        println!("{}", json!({"url": url}));
    }
    Command::new("open").arg(&url).spawn()?;
    if !json_mode {
        println!("opened {url}");
    }
    Ok(0)
}

pub fn logs(name: &str, which: &str, follow: bool) -> Result<i32> {
    let file = match which {
        "mysql" => paths::logs_dir().join("mysql.log"),
        "router" => paths::logs_dir().join("ferrod.log"),
        _ => paths::logs_dir().join(format!("{name}-php.log")),
    };
    if !file.exists() {
        bail!("no log at {} yet", file.display());
    }
    let mut tail = Command::new("tail");
    if follow {
        tail.args(["-f"]);
    } else {
        tail.args(["-n", "100"]);
    }
    let status = tail.arg(&file).status()?;
    Ok(status.code().unwrap_or(0))
}

pub fn trust_ca() -> Result<i32> {
    let ca_pem = paths::ca_dir().join("ferro-ca.pem");
    if !ca_pem.exists() {
        bail!(
            "no CA at {} — start the daemon first (`ferro daemon start`)",
            ca_pem.display()
        );
    }
    println!("Ferro serves HTTPS with a local root CA at:");
    println!("  {}", ca_pem.display());
    println!();
    println!("Trusting it means sites at https://<name>.localhost stop showing");
    println!("browser warnings. macOS will prompt for your password to modify");
    println!("the login keychain. Only localhost certs are ever issued by this CA.");
    println!();
    print!("Add it to your login keychain as a trusted root? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("not trusted — nothing changed");
        return Ok(0);
    }
    let keychain = paths::home_dir().join("Library/Keychains/login.keychain-db");
    let status = Command::new("security")
        .args(["add-trusted-cert", "-d", "-r", "trustRoot", "-k"])
        .arg(&keychain)
        .arg(&ca_pem)
        .status()
        .context("failed to run `security add-trusted-cert`")?;
    if status.success() {
        println!("CA trusted — reload your browser tabs");
        Ok(0)
    } else {
        bail!("security add-trusted-cert failed");
    }
}

/// `ferro php <name> <version>` — switch a site's PHP version.
pub async fn set_php(name: &str, version: &str, json_mode: bool) -> Result<i32> {
    call("site.set_php", json!({"name": name, "version": version})).await?;
    if json_mode {
        println!("{}", json!({"name": name, "php_version": version}));
    } else {
        println!("{name} now runs PHP {version}");
    }
    Ok(0)
}

/// `ferro mail` — open the Mailpit UI (mail.localhost via the router).
pub fn mail() -> Result<i32> {
    if !paths::bin_dir().join("mailpit").exists() {
        bail!("mailpit is not installed — run `ferro update mailpit`");
    }
    let config = Config::load();
    let url = format!("https://mail.localhost:{}", config.https_port);
    Command::new("open").arg(&url).spawn()?;
    println!("opened {url}");
    Ok(0)
}
