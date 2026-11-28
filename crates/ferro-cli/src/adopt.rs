//! `ferro adopt` — take over an existing WordPress folder (exported from
//! Local, MAMP, a backup, or a previous ferro install) and serve it as a
//! ferro site: registers the folder in place, points wp-config.php at the
//! ferro database, installs the login/mail mu-plugins, and optionally
//! imports a SQL dump and rewrites its URLs.

use crate::{db, new, replace};
use anyhow::{bail, Context, Result};
use clap::Args;
use ferro_core::config::Config;
use ferro_core::ipc::call;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args, Clone)]
pub struct AdoptArgs {
    /// Site name — becomes https://<name>.localhost:<port>.
    pub name: String,
    /// Folder containing the existing WordPress install.
    pub path: String,
    /// Import this SQL dump into the site's (fresh) ferro database.
    #[arg(long)]
    pub sql: Option<String>,
    /// Previous URL of the site, rewritten to the local one after an
    /// import (default: read from the imported database).
    #[arg(long)]
    pub from: Option<String>,
    /// PHP version for the site (default: the configured default).
    #[arg(long)]
    pub php: Option<String>,
}

pub async fn adopt(args: AdoptArgs, json_mode: bool) -> Result<i32> {
    ferro_core::site::Site::validate_name(&args.name)
        .map_err(|e| anyhow::anyhow!("invalid site name: {e}"))?;

    let existing = call("site.list", json!({})).await?;
    if existing
        .as_array()
        .map(|sites| {
            sites
                .iter()
                .any(|s| s.get("name").and_then(|n| n.as_str()) == Some(args.name.as_str()))
        })
        .unwrap_or(false)
    {
        bail!("a site named `{}` already exists", args.name);
    }

    let path = PathBuf::from(&args.path)
        .canonicalize()
        .with_context(|| format!("{} does not exist", args.path))?;
    if !path.is_dir() {
        bail!("{} is not a folder", path.display());
    }
    if !path.join("wp-load.php").exists() {
        bail!(
            "{} does not look like a WordPress root (no wp-load.php)",
            path.display()
        );
    }
    for protected in ["Desktop", "Documents", "Downloads"] {
        let dir = ferro_core::paths::home_dir().join(protected);
        if path.starts_with(&dir) {
            eprintln!(
                "  ! warning: macOS privacy protection may block ferrod (a background\n  \
                 ! daemon) from reading ~/{protected}. If the site 404s, move the folder\n  \
                 ! elsewhere (e.g. ~/Sites) or grant Full Disk Access to ferrod."
            );
            break;
        }
    }

    let config = Config::load();
    let php_version = args.php.clone().unwrap_or(config.default_php.clone());
    let url = format!("https://{}.localhost:{}", args.name, config.https_port);

    new::step(json_mode, "registering site");
    call(
        "site.register",
        json!({
            "name": args.name,
            "path": path,
            "kind": "linked",
            "php": php_version,
        }),
    )
    .await?;

    // Anything failing after registration would leave a phantom site —
    // roll back the registration (and its fresh database) in that case.
    if let Err(err) = wire_site(&args, &path, &php_version, &url, json_mode).await {
        let _ = call(
            "site.unregister",
            json!({"name": args.name, "drop_db": true}),
        )
        .await;
        return Err(err);
    }

    if json_mode {
        println!(
            "{}",
            json!({"name": args.name, "url": url, "path": path, "php_version": php_version})
        );
    } else {
        println!();
        println!("✓ {} is being served from {}", args.name, path.display());
        println!("  site:   {url}");
        println!("  admin:  {url}/wp-admin/  (one-click: `ferro open {} --admin`)", args.name);
        if args.sql.is_none() {
            println!();
            println!("  note: no database was imported — the site uses a fresh empty");
            println!("  database. Import one with: ferro db {} import dump.sql", args.name);
        }
    }
    Ok(0)
}

/// Rewire wp-config.php, install mu-plugins, import the dump and
/// rewrite URLs — everything after registration.
async fn wire_site(
    args: &AdoptArgs,
    path: &std::path::Path,
    php_version: &str,
    url: &str,
    json_mode: bool,
) -> Result<()> {
    let info = call("site.info", json!({"name": args.name})).await?;
    let site = info.get("site").context("bad site.info reply")?;
    let db_name = site.get("db_name").and_then(|v| v.as_str()).unwrap();
    let db_user = site.get("db_user").and_then(|v| v.as_str()).unwrap();
    let db_pass = site.get("db_pass").and_then(|v| v.as_str()).unwrap();
    let db_host = format!(
        "localhost:{}",
        ferro_core::wpcli::mysql_socket().display()
    );

    new::step(json_mode, "pointing wp-config.php at the ferro database");
    if path.join("wp-config.php").exists() {
        for (key, value) in [
            ("DB_NAME", db_name.to_string()),
            ("DB_USER", db_user.to_string()),
            ("DB_PASSWORD", db_pass.to_string()),
            ("DB_HOST", db_host),
        ] {
            new::wp(
                php_version,
                path,
                &["config".into(), "set".into(), key.into(), value],
            )?;
        }
        new::wp(
            php_version,
            path,
            &[
                "config".into(),
                "set".into(),
                "FERRO".into(),
                "true".into(),
                "--raw".into(),
            ],
        )?;
    } else {
        new::wp_stdin(
            php_version,
            path,
            &[
                "config".into(),
                "create".into(),
                format!("--dbname={db_name}"),
                format!("--dbuser={db_user}"),
                format!("--dbpass={db_pass}"),
                format!("--dbhost={db_host}"),
                "--extra-php".into(),
            ],
            Some("define('FERRO', true);\ndefine('WP_ENVIRONMENT_TYPE', 'local');\n"),
        )?;
    }

    new::step(json_mode, "installing ferro mu-plugins");
    let mu_dir = path.join("wp-content/mu-plugins");
    std::fs::create_dir_all(&mu_dir)?;
    std::fs::write(mu_dir.join("ferro-login.php"), new::MU_PLUGIN)?;
    std::fs::write(mu_dir.join("ferro-mail.php"), new::MAIL_PLUGIN)?;

    if let Some(sql) = &args.sql {
        new::step(json_mode, "importing database dump");
        db::import(&args.name, sql).await?;
    }

    // Rewrite URLs when we know (or can discover) the old one.
    let old_url = match &args.from {
        Some(from) => Some(from.trim_end_matches('/').to_string()),
        None if args.sql.is_some() => db_home_url(php_version, path),
        None => None,
    };
    if let Some(old_url) = old_url {
        if old_url != url {
            new::step(json_mode, &format!("rewriting URLs ({old_url} -> {url})"));
            replace::replace(&args.name, &old_url, url, "rust", json_mode).await?;
            // Cover the other scheme too (http vs https).
            let variant = if let Some(rest) = old_url.strip_prefix("https://") {
                Some(format!("http://{rest}"))
            } else {
                old_url
                    .strip_prefix("http://")
                    .map(|rest| format!("https://{rest}"))
            };
            if let Some(variant) = variant {
                let _ = replace::replace(&args.name, &variant, url, "rust", json_mode).await;
            }
        }
        // A hardcoded WP_HOME/WP_SITEURL from the old environment would
        // override the database — pin both to the local URL.
        for key in ["WP_HOME", "WP_SITEURL"] {
            new::wp(
                php_version,
                path,
                &["config".into(), "set".into(), key.into(), url.to_string()],
            )?;
        }
    }
    Ok(())
}

/// `wp option get home`, tolerating an empty/broken database.
fn db_home_url(php_version: &str, path: &std::path::Path) -> Option<String> {
    let output = ferro_core::wpcli::wp_command(
        php_version,
        path,
        &["option".into(), "get".into(), "home".into()],
    )
    .output()
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (home.starts_with("http")).then(|| home.trim_end_matches('/').to_string())
}
