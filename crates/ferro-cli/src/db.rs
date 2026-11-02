use anyhow::{bail, Context, Result};
use ferro_core::config::Config;
use ferro_core::ipc::call;
use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn mysqldump() -> PathBuf {
    ferro_core::paths::bin_dir().join("mysql/bin/mysqldump")
}

struct DbInfo {
    user: String,
    pass: String,
    name: String,
}

async fn db_info(name: &str) -> Result<(DbInfo, serde_json::Value)> {
    let value = call("site.info", json!({"name": name})).await?;
    let site = value
        .get("site")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("bad site.info reply"))?;
    let get = |key: &str| {
        site.get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
            .with_context(|| format!("site `{name}` has no database ({key} missing)"))
    };
    Ok((
        DbInfo {
            user: get("db_user")?,
            pass: get("db_pass")?,
            name: get("db_name")?,
        },
        value,
    ))
}

fn socket() -> String {
    ferro_core::wpcli::mysql_socket().display().to_string()
}

pub async fn export(name: &str, file: &str) -> Result<i32> {
    let (db, _) = db_info(name).await?;
    let out = std::fs::File::create(file).with_context(|| format!("could not create {file}"))?;
    let status = Command::new(mysqldump())
        .arg(format!("--socket={}", socket()))
        .arg(format!("-u{}", db.user))
        .arg(format!("-p{}", db.pass))
        .args(["--single-transaction", "--quick", "--set-gtid-purged=OFF"])
        .arg(&db.name)
        .stdout(Stdio::from(out))
        .status()
        .context("could not run mysqldump")?;
    if !status.success() {
        bail!("mysqldump failed — see ~/Library/Logs/ferro/ for details");
    }
    println!("exported {} to {file}", db.name);
    Ok(0)
}

pub async fn import(name: &str, file: &str) -> Result<i32> {
    let (db, _) = db_info(name).await?;
    let input = std::fs::File::open(file).with_context(|| format!("could not open {file}"))?;
    let status = Command::new(ferro_core::wpcli::mysql_client())
        .arg(format!("--socket={}", socket()))
        .arg(format!("-u{}", db.user))
        .arg(format!("-p{}", db.pass))
        .arg(&db.name)
        .stdin(Stdio::from(input))
        .status()
        .context("could not run mysql")?;
    if !status.success() {
        bail!("import failed");
    }
    println!("imported {file} into {}", db.name);
    Ok(0)
}

pub async fn shell(name: &str) -> Result<i32> {
    let (db, _) = db_info(name).await?;
    let err = std::os::unix::process::CommandExt::exec(
        Command::new(ferro_core::wpcli::mysql_client())
            .arg(format!("--socket={}", socket()))
            .arg(format!("-u{}", db.user))
            .arg(format!("-p{}", db.pass))
            .arg(&db.name),
    );
    bail!("could not exec mysql: {err}");
}

pub async fn open(name: &str) -> Result<i32> {
    let config = Config::load();
    // Touch the site so a missing name errors before we open a browser.
    let _ = db_info(name).await?;
    let url = format!(
        "https://adminer.localhost:{}/?site={name}",
        config.https_port
    );
    Command::new("open").arg(&url).spawn()?;
    println!("opened {url}");
    Ok(0)
}

pub async fn adminer() -> Result<i32> {
    let config = Config::load();
    let url = format!("https://adminer.localhost:{}/", config.https_port);
    Command::new("open").arg(&url).spawn()?;
    println!("opened {url}");
    Ok(0)
}

pub async fn info(name: &str, show_password: bool, json_mode: bool) -> Result<i32> {
    let (db, raw) = db_info(name).await?;
    let config = Config::load();
    let socket_path = raw
        .get("db_socket")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(socket);
    if json_mode {
        let mut value = json!({
            "site": name,
            "host": "127.0.0.1",
            "port": config.mysql_port,
            "socket": socket_path,
            "database": db.name,
            "user": db.user,
        });
        if show_password {
            value
                .as_object_mut()
                .unwrap()
                .insert("password".into(), json!(db.pass));
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("connection info for `{name}` (GUI clients like TablePlus):");
        println!("  host:     127.0.0.1 (or socket {})", socket_path);
        println!("  port:     {}", config.mysql_port);
        println!("  database: {}", db.name);
        println!("  user:     {}", db.user);
        if show_password {
            println!("  password: {}", db.pass);
        } else {
            println!("  password: (hidden — pass --show-password)");
        }
    }
    Ok(0)
}
