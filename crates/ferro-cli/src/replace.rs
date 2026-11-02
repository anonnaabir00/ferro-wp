use anyhow::{bail, Context, Result};
use ferro_core::ipc::call;
use ferro_replace::Replacer;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

/// mysqldump of `src_db` -> replacer -> mysql into `dst_db`, one
/// streaming pipeline. Runs as root over the socket.
fn replace_rust_pipeline(
    socket: &str,
    src_db: &str,
    dst_db: &str,
    from: &str,
    to: &str,
) -> Result<u64> {
    let replacer = Replacer::new(&Replacer::url_variants(from, to));

    let mut dump = Command::new(ferro_core::paths::bin_dir().join("mysql/bin/mysqldump"))
        .arg(socket)
        .arg("-uroot")
        .args([
            "--single-transaction",
            "--quick",
            "--set-gtid-purged=OFF",
            "--no-tablespaces",
        ])
        .arg(src_db)
        .stdout(Stdio::piped())
        .spawn()
        .context("could not start mysqldump")?;
    let dump_stdout = dump.stdout.take().context("mysqldump stdout missing")?;

    let mut mysql = Command::new(ferro_core::wpcli::mysql_client())
        .arg(socket)
        .arg("-uroot")
        .arg(dst_db)
        .stdin(Stdio::piped())
        .spawn()
        .context("could not start mysql")?;
    let mut mysql_stdin = mysql.stdin.take().context("mysql stdin missing")?;

    let pump = std::thread::spawn(move || -> std::io::Result<u64> {
        mysql_stdin.write_all(b"SET foreign_key_checks=0;\n")?;
        let written = replacer.replace_stream(dump_stdout, &mut mysql_stdin, true)?;
        mysql_stdin.write_all(b"\nSET foreign_key_checks=1;\n")?;
        Ok(written)
    });

    let written = pump
        .join()
        .map_err(|_| anyhow::anyhow!("replace pipeline panicked"))??;
    let dump_status = dump.wait()?;
    let mysql_status = mysql.wait()?;
    if !dump_status.success() || !mysql_status.success() {
        bail!("replace pipeline failed (dump: {dump_status}, import: {mysql_status})");
    }
    Ok(written)
}
/// `ferro replace <site> <from> <to> [--engine rust|wp-cli]`
///
/// Rust engine (default): mysqldump | ferro-replace | mysql into a
/// shadow database, then an atomic table swap.
pub async fn replace(
    name: &str,
    from: &str,
    to: &str,
    engine: &str,
    json_mode: bool,
) -> Result<i32> {
    let started = Instant::now();
    let info = call("site.info", json!({"name": name})).await?;
    let site = info.get("site").cloned().context("bad site.info reply")?;
    let php_version = site
        .get("php_version")
        .and_then(|v| v.as_str())
        .unwrap_or("8.3")
        .to_string();
    let path = std::path::PathBuf::from(site.get("path").and_then(|v| v.as_str()).unwrap());

    match engine {
        "wp-cli" => replace_wp_cli(&php_version, &path, from, to, json_mode, started).await,
        "rust" => replace_rust(&site, from, to, json_mode, started).await,
        other => bail!("unknown engine `{other}` (rust or wp-cli)"),
    }
}

async fn replace_wp_cli(
    php_version: &str,
    path: &std::path::Path,
    from: &str,
    to: &str,
    json_mode: bool,
    started: Instant,
) -> Result<i32> {
    let output = ferro_core::wpcli::wp_command(
        php_version,
        path,
        &["search-replace".into(), from.into(), to.into()],
    )
    .output()
    .context("could not run wp search-replace")?;
    if !output.status.success() {
        bail!(
            "wp search-replace failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if json_mode {
        println!(
            "{}",
            json!({"engine": "wp-cli", "elapsed_ms": started.elapsed().as_millis()})
        );
    } else {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(0)
}

/// Run SQL over the root socket connection (root has no password;
/// local-only). Used for the shadow-database swap, which needs
/// cross-database privileges the per-site user does not have.
fn root_sql(socket: &str, sql: &str) -> Result<()> {
    let out = Command::new(ferro_core::wpcli::mysql_client())
        .arg(socket)
        .arg("-uroot")
        .arg("-e")
        .arg(sql)
        .output()
        .context("could not run mysql")?;
    if !out.status.success() {
        bail!("mysql failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

async fn replace_rust(
    site: &serde_json::Value,
    from: &str,
    to: &str,
    json_mode: bool,
    started: Instant,
) -> Result<i32> {
    let get = |key: &str| {
        site.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let socket = format!("--socket={}", ferro_core::wpcli::mysql_socket().display());
    let db = get("db_name");
    // Stream into a shadow database and swap tables at the end: the
    // dump's consistent read holds metadata locks on the source
    // tables, so importing DROP/CREATE into the same database while
    // the dump is still reading deadlocks. The swap also means the
    // original data survives a failed replace.
    let new_db = format!("{db}__replace_new");
    let old_db = format!("{db}__replace_old");
    root_sql(
        &socket,
        &format!("DROP DATABASE IF EXISTS `{new_db}`; CREATE DATABASE `{new_db}`"),
    )?;

    let written = match replace_rust_pipeline(&socket, &db, &new_db, from, to) {
        Ok(w) => w,
        Err(err) => {
            let _ = root_sql(&socket, &format!("DROP DATABASE IF EXISTS `{new_db}`"));
            return Err(err);
        }
    };

    // Atomic swap: current tables aside, shadow tables in.
    let tables_out = Command::new(ferro_core::wpcli::mysql_client())
        .arg(&socket)
        .arg("-uroot")
        .arg("-N")
        .arg("-B")
        .arg("-e")
        .arg(format!(
            "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA='{db}'"
        ))
        .output()
        .context("could not list tables")?;
    if !tables_out.status.success() {
        bail!(
            "could not list tables: {}",
            String::from_utf8_lossy(&tables_out.stderr)
        );
    }
    let tables: Vec<String> = String::from_utf8_lossy(&tables_out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let mut rename = String::from("RENAME TABLE ");
    let mut first = true;
    for t in &tables {
        if !first {
            rename.push_str(", ");
        }
        first = false;
        rename.push_str(&format!("`{db}`.`{t}` TO `{old_db}`.`{t}`"));
    }
    for t in &tables {
        rename.push_str(&format!(", `{new_db}`.`{t}` TO `{db}`.`{t}`"));
    }
    if !tables.is_empty() {
        root_sql(
            &socket,
            &format!("DROP DATABASE IF EXISTS `{old_db}`; CREATE DATABASE `{old_db}`"),
        )?;
        root_sql(&socket, &rename)?;
        root_sql(&socket, &format!("DROP DATABASE `{old_db}`"))?;
    }
    root_sql(&socket, &format!("DROP DATABASE `{new_db}`"))?;

    if json_mode {
        println!(
            "{}",
            json!({
                "engine": "rust",
                "bytes": written,
                "elapsed_ms": started.elapsed().as_millis(),
            })
        );
    } else {
        println!(
            "replaced {} -> {} ({:.1} MB, {:.1}s)",
            from,
            to,
            written as f64 / 1_048_576.0,
            started.elapsed().as_secs_f32()
        );
    }
    Ok(0)
}
