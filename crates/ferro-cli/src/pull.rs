//! `ferro pull <ssh-target> <remote-path>` — pull a production
//! WordPress site to local: stream its database over SSH (replacing
//! URLs on the way through), rsync the files without uploads, and point
//! missing media at the origin through the media proxy.

use crate::new::{step, wp, MU_PLUGIN};
use anyhow::{bail, Context, Result};
use clap::Args;
use ferro_core::config::Config;
use ferro_core::ipc::call;
use ferro_core::{paths, wpcli};
use ferro_import::{rsync_files, ssh_capture, Compression, RemoteWpConfig};
use ferro_replace::Replacer;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Args, Clone)]
pub struct PullArgs {
    /// SSH target (host alias from ~/.ssh/config, user@host, ...).
    pub target: String,
    /// Absolute path of the WordPress root on the remote.
    pub remote_path: String,
    /// Local site name (default: remote folder name, DNS-sanitized).
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub php: Option<String>,
    /// Do not configure the media proxy for missing uploads.
    #[arg(long)]
    pub no_media_proxy: bool,
    /// Also rsync wp-content/uploads (default: leave on the origin).
    #[arg(long)]
    pub include_uploads: bool,
    /// Media proxy mode: 302 redirect (default) or stream via the router.
    #[arg(long, default_value = "redirect")]
    pub media_mode: String,
    /// With --media-mode=proxy: save proxied files locally on first hit.
    #[arg(long)]
    pub cache_media: bool,
    /// Extra plugins to deactivate locally (comma-separated slugs).
    #[arg(long)]
    pub deactivate: Option<String>,
    /// Keep all plugins active.
    #[arg(long)]
    pub no_deactivate: bool,
}

/// Plugins that typically break or are pointless locally (caching,
/// security, CDN). Deactivated unless --no-deactivate.
const DEFAULT_DEACTIVATE: &[&str] = &[
    "w3-total-cache",
    "wp-super-cache",
    "litespeed-cache",
    "wp-fastest-cache",
    "autoptimize",
    "cloudflare",
    "wordfence",
    "jetpack-boost",
];

/// Drop-in files left by caching/DB plugins that break locally.
const DROPINS: &[&str] = &["advanced-cache.php", "object-cache.php", "db.php"];

struct Stage {
    name: &'static str,
    started: Instant,
}

impl Stage {
    fn begin(json_mode: bool, name: &'static str) -> Self {
        step(json_mode, name);
        Stage {
            name,
            started: Instant::now(),
        }
    }

    fn end(self, timings: &mut Vec<(String, f32)>) {
        timings.push((self.name.to_string(), self.started.elapsed().as_secs_f32()));
    }
}

pub async fn pull(args: PullArgs, json_mode: bool) -> Result<i32> {
    let total = Instant::now();
    let mut timings: Vec<(String, f32)> = Vec::new();
    let config = Config::load();

    // --- probe -------------------------------------------------------------
    let stage = Stage::begin(json_mode, "probing remote");
    let remote_root = args.remote_path.trim_end_matches('/').to_string();
    let wp_config_text = ssh_capture(
        &args.target,
        &format!("cat {}/wp-config.php", shell_quote(&remote_root)),
    )
    .context(
        "could not read wp-config.php on the remote — is that a WordPress root, \
         and does `ssh <target>` work (BatchMode: keys/agent must be set up)?",
    )?;
    let remote = RemoteWpConfig::parse(&wp_config_text)
        .context("could not parse the remote wp-config.php")?;
    stage.end(&mut timings);

    // --- site url ----------------------------------------------------------
    let stage = Stage::begin(json_mode, "reading remote site url");
    let prod_url = remote_site_url(&args.target, &remote_root, &remote)?;
    if !prod_url.starts_with("http") {
        bail!("remote site url looks wrong: `{prod_url}`");
    }
    stage.end(&mut timings);

    // --- local site --------------------------------------------------------
    let name = match &args.name {
        Some(name) => name.clone(),
        None => dns_safe_name(
            Path::new(&remote_root)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("site"),
        ),
    };
    ferro_core::site::Site::validate_name(&name)
        .map_err(|e| anyhow::anyhow!("invalid site name `{name}`: {e} — pass --name"))?;
    let local_url = format!("https://{name}.localhost:{}", config.https_port);

    let existing = call("site.list", json!({})).await?;
    if existing
        .as_array()
        .map(|sites| {
            sites
                .iter()
                .any(|s| s.get("name").and_then(|n| n.as_str()) == Some(name.as_str()))
        })
        .unwrap_or(false)
    {
        bail!("a site named `{name}` already exists — `ferro destroy {name}` first");
    }
    let path = config
        .parked_paths
        .first()
        .cloned()
        .unwrap_or_else(|| paths::home_dir().join("Sites"))
        .join(&name);
    std::fs::create_dir_all(&path)?;
    let php_version = args.php.clone().unwrap_or(config.default_php.clone());

    let stage = Stage::begin(json_mode, "creating local site + database");
    call(
        "site.register",
        json!({
            "name": name,
            "path": path,
            "kind": "created",
            "php": php_version,
        }),
    )
    .await?;
    let info = call("site.info", json!({"name": name})).await?;
    let site = info.get("site").cloned().context("bad site.info reply")?;
    stage.end(&mut timings);

    let pulled = pull_rest(
        &args,
        &remote,
        &prod_url,
        &name,
        &path,
        &site,
        &php_version,
        &local_url,
        json_mode,
        &mut timings,
    )
    .await;
    if let Err(err) = pulled {
        // Leave a clean slate rather than a half-pulled site.
        let _ = call("site.unregister", json!({"name": name, "drop_db": true})).await;
        let _ = std::fs::remove_dir_all(&path);
        return Err(err);
    }

    // --- report ------------------------------------------------------------
    let elapsed = total.elapsed().as_secs_f32();
    notify(
        "Ferro WP",
        &format!("Pulled {} → {}", args.target, local_url),
    );
    if json_mode {
        println!(
            "{}",
            json!({
                "name": name,
                "url": local_url,
                "from": prod_url,
                "elapsed_s": elapsed,
                "stages": timings.iter().map(|(n, s)| json!({"stage": n, "s": s})).collect::<Vec<_>>(),
            })
        );
    } else {
        println!();
        println!(
            "✓ pulled {} -> {} ({elapsed:.1}s total)",
            args.target, local_url
        );
        for (stage, secs) in &timings {
            println!("  {secs:>7.1}s  {stage}");
        }
        println!("  admin:  {local_url}/wp-admin/ (production credentials)");
        if !args.no_media_proxy {
            println!(
                "  media:  missing uploads are served from {prod_url} ({} mode)",
                args.media_mode
            );
        }
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
async fn pull_rest(
    args: &PullArgs,
    remote: &RemoteWpConfig,
    prod_url: &str,
    name: &str,
    path: &Path,
    site: &serde_json::Value,
    php_version: &str,
    local_url: &str,
    json_mode: bool,
    timings: &mut Vec<(String, f32)>,
) -> Result<()> {
    // --- files -------------------------------------------------------------
    let stage = Stage::begin(json_mode, "syncing files (rsync)");
    let remote_path = PathBuf::from(args.remote_path.trim_end_matches('/'));
    rsync_files(&args.target, &remote_path, path, args.include_uploads)?;
    stage.end(timings);

    // --- database ----------------------------------------------------------
    let stage = Stage::begin(json_mode, "importing database (stream + replace)");
    stream_pull_import(args, remote, prod_url, name, site)?;
    stage.end(timings);

    // --- wp-config ---------------------------------------------------------
    let stage = Stage::begin(json_mode, "rewriting wp-config.php");
    let db_name = site.get("db_name").and_then(|v| v.as_str()).unwrap();
    let db_user = site.get("db_user").and_then(|v| v.as_str()).unwrap();
    let db_pass = site.get("db_pass").and_then(|v| v.as_str()).unwrap();
    let sets: Vec<(String, String, bool)> = vec![
        ("DB_NAME".into(), db_name.into(), false),
        ("DB_USER".into(), db_user.into(), false),
        ("DB_PASSWORD".into(), db_pass.into(), false),
        (
            "DB_HOST".into(),
            format!("localhost:{}", wpcli::mysql_socket().display()),
            false,
        ),
        ("WP_HOME".into(), local_url.into(), false),
        ("WP_SITEURL".into(), local_url.into(), false),
        ("WP_ENVIRONMENT_TYPE".into(), "local".into(), false),
        ("WP_DEBUG".into(), "true".into(), true),
        ("WP_DEBUG_LOG".into(), "true".into(), true),
        ("WP_DEBUG_DISPLAY".into(), "false".into(), true),
        ("FERRO".into(), "true".into(), true),
    ];
    for (key, value, raw) in sets {
        let mut cmd = vec!["config".into(), "set".into(), key, value];
        if raw {
            cmd.push("--raw".into());
        }
        wp(php_version, path, &cmd)?;
    }
    // table_prefix: keep the remote one — the imported tables use it.
    if remote.table_prefix != "wp_" {
        wp(
            php_version,
            path,
            &[
                "config".into(),
                "set".into(),
                "table_prefix".into(),
                remote.table_prefix.clone(),
            ],
        )?;
    }
    for dropin in DROPINS {
        let file = path.join("wp-content").join(dropin);
        if file.exists() {
            std::fs::rename(&file, file.with_extension("php.ferro-off"))?;
        }
    }
    stage.end(timings);

    // --- plugins + login ----------------------------------------------------
    let stage = Stage::begin(json_mode, "deactivating hostile plugins");
    if !args.no_deactivate {
        let mut slugs: Vec<String> = DEFAULT_DEACTIVATE.iter().map(|s| s.to_string()).collect();
        if let Some(extra) = &args.deactivate {
            slugs.extend(extra.split(',').map(|s| s.trim().to_string()));
        }
        // Only deactivate what's actually installed.
        let installed = wp_output(
            php_version,
            path,
            &["plugin".into(), "list".into(), "--field=name".into()],
        )
        .unwrap_or_default();
        let present: Vec<String> = slugs
            .into_iter()
            .filter(|slug| installed.lines().any(|line| line.trim() == slug))
            .collect();
        if !present.is_empty() {
            let mut cmd = vec!["plugin".into(), "deactivate".into()];
            cmd.extend(present);
            let _ = wp(php_version, path, &cmd); // best effort
        }
    }
    let mu_dir = path.join("wp-content/mu-plugins");
    std::fs::create_dir_all(&mu_dir)?;
    std::fs::write(mu_dir.join("ferro-login.php"), MU_PLUGIN)?;
    stage.end(timings);

    // --- media proxy --------------------------------------------------------
    if !args.no_media_proxy && !args.include_uploads {
        let stage = Stage::begin(json_mode, "configuring media proxy");
        call(
            "site.set_media_proxy",
            json!({
                "name": name,
                "origin": prod_url,
                "mode": args.media_mode,
                "cache": args.cache_media,
            }),
        )
        .await?;
        stage.end(timings);
    }
    Ok(())
}

/// ssh → mysqldump → decompress → ferro-replace → local mysql, with
/// InnoDB import tuning restored afterwards.
fn stream_pull_import(
    args: &PullArgs,
    remote: &RemoteWpConfig,
    prod_url: &str,
    name: &str,
    site: &serde_json::Value,
) -> Result<()> {
    let socket = format!("--socket={}", wpcli::mysql_socket().display());
    let compression = Compression::negotiate(&args.target);

    // Tune for bulk import; remember previous values to restore.
    let tune = "SELECT @@global.innodb_flush_log_at_trx_commit, @@global.sync_binlog";
    let previous = root_query(&socket, tune).unwrap_or_else(|_| "1\t0".to_string());
    root_sql(
        &socket,
        "SET GLOBAL innodb_flush_log_at_trx_commit=2; SET GLOBAL sync_binlog=0",
    )?;

    let result = (|| -> Result<()> {
        let dump_args = remote
            .mysql_args("mysqldump")
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let remote_cmd = format!("mysqldump {dump_args}{}", compression.remote_pipe());
        let mut dump = ferro_import::ssh_command(&args.target, &remote_cmd)
            .stdout(Stdio::piped())
            .spawn()
            .context("could not start ssh mysqldump")?;
        let dump_stdout = dump.stdout.take().context("ssh stdout missing")?;

        let (reader, decompressor): (Box<dyn std::io::Read + Send>, Option<std::process::Child>) =
            match compression.local_binary() {
                Some(bin) => {
                    let mut child = Command::new(bin)
                        .arg("-d")
                        .stdin(Stdio::from(dump_stdout))
                        .stdout(Stdio::piped())
                        .spawn()
                        .expect("decompressor spawns (negotiated earlier)");
                    (Box::new(child.stdout.take().unwrap()), Some(child))
                }
                None => (Box::new(dump_stdout), None),
            };

        let db_name = site.get("db_name").and_then(|v| v.as_str()).unwrap();
        let mut mysql = Command::new(wpcli::mysql_client())
            .arg(&socket)
            .arg("-uroot")
            .arg(db_name)
            .stdin(Stdio::piped())
            .spawn()
            .context("could not start mysql")?;
        let mut mysql_stdin = mysql.stdin.take().context("mysql stdin missing")?;

        let local_url = local_url_of(name);
        let replacer = Replacer::new(&Replacer::url_variants(prod_url, &local_url));
        let pump = std::thread::spawn(move || -> std::io::Result<u64> {
            mysql_stdin.write_all(
                b"SET foreign_key_checks=0;\nSET unique_checks=0;\nSET sql_log_bin=0;\n",
            )?;
            let written = replacer.replace_stream(reader, &mut mysql_stdin, true)?;
            mysql_stdin.write_all(b"\nSET foreign_key_checks=1;\nSET unique_checks=1;\n")?;
            Ok(written)
        });
        let written = pump
            .join()
            .map_err(|_| anyhow::anyhow!("pull import panicked"))??;
        let dump_status = dump.wait()?;
        if let Some(mut dec) = decompressor {
            let _ = dec.wait();
        }
        let mysql_status = mysql.wait()?;
        if !dump_status.success() || !mysql_status.success() {
            bail!(
                "database pull failed (ssh dump: {dump_status}, import: {mysql_status}) — \
                 is mysqldump available on the remote?"
            );
        }
        eprintln!(
            "  .. imported {:.1} MB ({})",
            written as f64 / 1_048_576.0,
            match compression {
                Compression::Zstd => "zstd",
                Compression::Gzip => "gzip",
                Compression::None => "uncompressed",
            }
        );
        Ok(())
    })();

    // Restore the server settings no matter how the import went.
    let mut parts = previous.split('\t');
    let flush = parts.next().unwrap_or("1").trim();
    let binlog = parts.next().unwrap_or("0").trim();
    let _ = root_sql(
        &socket,
        &format!(
            "SET GLOBAL innodb_flush_log_at_trx_commit={flush}; SET GLOBAL sync_binlog={binlog}"
        ),
    );
    result
}

/// Remote siteurl: wp-cli when present, else a direct mysql query
/// against the credentials from wp-config.php.
fn remote_site_url(target: &str, remote_root: &str, config: &RemoteWpConfig) -> Result<String> {
    let quoted = shell_quote(remote_root);
    let via_wp = ssh_capture(
        target,
        &format!(
            "cd {quoted} && command -v wp >/dev/null && wp option get siteurl --skip-plugins --skip-themes 2>/dev/null || true"
        ),
    )
    .unwrap_or_default();
    let url = via_wp.trim();
    if url.starts_with("http") {
        return Ok(url.trim_end_matches('/').to_string());
    }
    let sql = format!(
        "SELECT option_value FROM {}options WHERE option_name='siteurl'",
        config.table_prefix
    );
    let mysql_args = config
        .mysql_args("mysql")
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let out = ssh_capture(
        target,
        &format!("mysql {mysql_args} -N -B -e {}", shell_quote(&sql)),
    )
    .context("could not read siteurl on the remote (no wp-cli and the mysql query failed)")?;
    let url = out.trim().trim_end_matches('/');
    if url.is_empty() {
        bail!("remote siteurl is empty");
    }
    Ok(url.to_string())
}

fn local_url_of(name: &str) -> String {
    let config = Config::load();
    format!("https://{name}.localhost:{}", config.https_port)
}

/// Turn an arbitrary folder name into a DNS-safe site name.
fn dns_safe_name(raw: &str) -> String {
    let mut name = raw.to_lowercase();
    name = name
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if name.is_empty() {
        name = "pulled".to_string();
    }
    name
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Run wp-cli and return stdout (empty on failure).
fn wp_output(php_version: &str, path: &Path, args: &[String]) -> Result<String> {
    let output = wpcli::wp_command(php_version, path, args)
        .output()
        .context("could not run wp-cli")?;
    if !output.status.success() {
        bail!("wp failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn root_sql(socket: &str, sql: &str) -> Result<()> {
    let out = Command::new(wpcli::mysql_client())
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

fn root_query(socket: &str, sql: &str) -> Result<String> {
    let out = Command::new(wpcli::mysql_client())
        .arg(socket)
        .arg("-uroot")
        .arg("-N")
        .arg("-B")
        .arg("-e")
        .arg(sql)
        .output()
        .context("could not run mysql")?;
    if !out.status.success() {
        bail!("mysql failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn notify(title: &str, message: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_quote(message),
        applescript_quote(title)
    );
    let _ = Command::new("osascript").arg("-e").arg(script).status();
}

fn applescript_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
