use anyhow::{bail, Context, Result};
use clap::Args;
use ferro_core::config::Config;
use ferro_core::ipc::call;
use ferro_core::{keychain, paths};
use serde::Deserialize;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// The one-click-login mu-plugin source, embedded so installs never
/// depend on the repo checkout.
const MU_PLUGIN: &str = include_str!("../../../assets/mu-plugins/ferro-login.php");

/// CLI arguments for `ferro new` (boxed in the Command enum).
#[derive(Args, Clone)]
pub struct NewArgs {
    pub name: String,
    #[arg(long)]
    pub php: Option<String>,
    /// WordPress version: latest, a version like 6.8, or nightly.
    #[arg(long, default_value = "latest")]
    pub wp: String,
    #[arg(long, default_value = "en_US")]
    pub locale: String,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, default_value = "admin")]
    pub admin_user: String,
    #[arg(long)]
    pub admin_email: Option<String>,
    /// Defaults to a generated password stored in the Keychain.
    #[arg(long)]
    pub admin_password: Option<String>,
    /// Multisite: subdirectory (default) or subdomain.
    #[arg(long, value_name = "TYPE", default_missing_value = "subdirectory", num_args = 0..=1)]
    pub multisite: Option<String>,
    /// Folder for the site (default: first parked folder/<name>).
    #[arg(long)]
    pub path: Option<String>,
    /// TOML blueprint: plugins/themes/options/commands to apply.
    #[arg(long)]
    pub blueprint: Option<String>,
    /// Folder, DB and PHP pool only — no WordPress.
    #[arg(long)]
    pub empty: bool,
}

impl From<NewArgs> for NewOptions {
    fn from(args: NewArgs) -> Self {
        NewOptions {
            name: args.name,
            php: args.php,
            wp: args.wp,
            locale: args.locale,
            title: args.title,
            admin_user: args.admin_user,
            admin_email: args.admin_email,
            admin_password: args.admin_password,
            multisite: args.multisite,
            path: args.path,
            blueprint: args.blueprint,
            empty: args.empty,
        }
    }
}

pub struct NewOptions {
    pub name: String,
    pub php: Option<String>,
    pub wp: String,
    pub locale: String,
    pub title: Option<String>,
    pub admin_user: String,
    pub admin_email: Option<String>,
    pub admin_password: Option<String>,
    pub multisite: Option<String>,
    pub path: Option<String>,
    pub blueprint: Option<String>,
    pub empty: bool,
}

pub async fn new(opts: NewOptions, json_mode: bool) -> Result<i32> {
    let started = Instant::now();
    ferro_core::site::Site::validate_name(&opts.name)
        .map_err(|e| anyhow::anyhow!("invalid site name: {e}"))?;

    // Uniqueness check.
    let existing = call("site.list", json!({})).await?;
    if existing
        .as_array()
        .map(|sites| {
            sites
                .iter()
                .any(|s| s.get("name").and_then(|n| n.as_str()) == Some(opts.name.as_str()))
        })
        .unwrap_or(false)
    {
        bail!(
            "a site named `{}` already exists — pick another name or `ferro destroy {}` first",
            opts.name,
            opts.name
        );
    }

    let config = Config::load();
    let path = match &opts.path {
        Some(p) => PathBuf::from(p),
        None => config
            .parked_paths
            .first()
            .cloned()
            .unwrap_or_else(|| paths::home_dir().join("Sites"))
            .join(&opts.name),
    };
    if path.exists() && path.read_dir()?.next().is_some() && !opts.empty {
        bail!(
            "{} is not empty — use --empty to take it over, or another name/path",
            path.display()
        );
    }
    std::fs::create_dir_all(&path)?;

    let php_version = opts.php.unwrap_or(config.default_php.clone());
    step(json_mode, "registering site");
    call(
        "site.register",
        json!({
            "name": opts.name,
            "path": path,
            "kind": "created",
            "php": php_version,
            "admin_user": opts.admin_user,
        }),
    )
    .await?;

    if opts.empty {
        println!("created empty site {} at {}", opts.name, path.display());
        println!(
            "database: ferro_{} (see `ferro db {} info`)",
            opts.name.replace('-', "_"),
            opts.name
        );
        return Ok(0);
    }

    // WordPress core from the local cache (download once per version/locale).
    step(json_mode, "fetching wordpress core");
    let zip = ensure_core(&opts.wp, &opts.locale)?;
    extract_core(&zip, &path)?;

    // DB credentials from the freshly registered site.
    let info = call("site.info", json!({"name": opts.name})).await?;
    let site = info.get("site").context("bad site.info reply")?;
    let db_name = site.get("db_name").and_then(|v| v.as_str()).unwrap();
    let db_user = site.get("db_user").and_then(|v| v.as_str()).unwrap();
    let db_pass = site.get("db_pass").and_then(|v| v.as_str()).unwrap();

    step(json_mode, "writing wp-config.php");
    wp_stdin(
        &php_version,
        &path,
        &[
            "config".into(),
            "create".into(),
            format!("--dbname={db_name}"),
            format!("--dbuser={db_user}"),
            format!("--dbpass={db_pass}"),
            format!(
                "--dbhost=localhost:{}",
                paths::app_data_dir().join("mysql/mysql.sock").display()
            ),
            "--extra-php".into(),
        ],
        Some(&wp_config_extra()),
    )?;

    let url = format!("https://{}.localhost:{}", opts.name, config.https_port);
    let title = opts.title.unwrap_or_else(|| title_case(&opts.name));
    let admin_email = opts
        .admin_email
        .unwrap_or_else(|| format!("admin@{}.localhost", opts.name));
    let admin_password = opts
        .admin_password
        .unwrap_or_else(|| ferro_core::random_token(12));

    step(json_mode, "installing wordpress");
    let mut install = vec![
        "core".into(),
        match &opts.multisite {
            Some(_) => "multisite-install".into(),
            None => "install".into(),
        },
        format!("--url={url}"),
        format!("--title={title}"),
        format!("--admin_user={}", opts.admin_user),
        format!("--admin_email={admin_email}"),
        format!("--admin_password={admin_password}"),
        format!("--locale={}", opts.locale),
        "--skip-email".into(),
    ];
    if matches!(opts.multisite.as_deref(), Some("subdomain")) {
        install.push("--subdomains".into());
    }
    wp(&php_version, &path, &install)?;

    step(json_mode, "tidying up");
    let _ = wp(
        &php_version,
        &path,
        &["rewrite".into(), "structure".into(), "/%postname%/".into()],
    );
    let _ = wp(
        &php_version,
        &path,
        &[
            "plugin".into(),
            "delete".into(),
            "hello".into(),
            "akismet".into(),
        ],
    );

    // One-click admin login mu-plugin.
    let mu_dir = path.join("wp-content/mu-plugins");
    std::fs::create_dir_all(&mu_dir)?;
    std::fs::write(mu_dir.join("ferro-login.php"), MU_PLUGIN)?;

    if let Some(blueprint) = &opts.blueprint {
        step(json_mode, "applying blueprint");
        apply_blueprint(&php_version, &path, Path::new(blueprint))?;
    }

    keychain::set_password(&opts.name, &admin_password)
        .context("could not store the admin password in the Keychain")?;

    if json_mode {
        println!(
            "{}",
            json!({
                "name": opts.name,
                "url": url,
                "admin_url": format!("{url}/wp-admin/"),
                "admin_user": opts.admin_user,
                "php_version": php_version,
                "elapsed_ms": started.elapsed().as_millis(),
            })
        );
    } else {
        println!();
        println!(
            "✓ {} is ready ({:.1}s)",
            opts.name,
            started.elapsed().as_secs_f32()
        );
        println!("  site:   {url}");
        println!("  admin:  {url}/wp-admin/  (user: {})", opts.admin_user);
        println!(
            "  password stored in Keychain — `ferro creds {}` to view",
            opts.name
        );
        println!("  one-click login: `ferro open {} --admin`", opts.name);
    }
    Ok(0)
}

pub async fn destroy(name: &str, yes: bool, json_mode: bool) -> Result<i32> {
    if !yes {
        eprint!(
            "destroy `{name}`? This drops its database and moves the folder to the Trash. [y/N] "
        );
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("aborted");
            return Ok(1);
        }
    }
    let info = call("site.info", json!({"name": name})).await?;
    let path = info
        .get("site")
        .and_then(|s| s.get("path"))
        .and_then(|p| p.as_str())
        .map(PathBuf::from);

    call("site.unregister", json!({"name": name, "drop_db": true})).await?;
    keychain::delete_password(name);

    let mut trashed = None;
    if let Some(path) = path {
        if path.exists() {
            let trash = paths::home_dir().join(".Trash");
            let mut target = trash.join(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
            let mut n = 1;
            while target.exists() {
                n += 1;
                target = trash.join(format!(
                    "{}-{n}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            std::fs::rename(&path, &target)
                .with_context(|| format!("could not move {} to Trash", path.display()))?;
            trashed = Some(target);
        }
    }
    if json_mode {
        println!("{}", json!({"destroyed": name, "trashed": trashed}));
    } else {
        println!("destroyed {name}: database dropped, cert/pool removed, folder in the Trash");
    }
    Ok(0)
}

pub async fn clone_site(name: &str, new_name: &str, json_mode: bool) -> Result<i32> {
    ferro_core::site::Site::validate_name(new_name)
        .map_err(|e| anyhow::anyhow!("invalid site name: {e}"))?;
    let config = Config::load();
    let info = call("site.info", json!({"name": name})).await?;
    let site = info.get("site").cloned().context("bad site.info reply")?;
    let src = PathBuf::from(site.get("path").and_then(|v| v.as_str()).unwrap());
    let php_version = site
        .get("php_version")
        .and_then(|v| v.as_str())
        .unwrap_or("8.3")
        .to_string();
    let dest = config
        .parked_paths
        .first()
        .cloned()
        .unwrap_or_else(|| paths::home_dir().join("Sites"))
        .join(new_name);
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }

    step(json_mode, "copying files");
    let status = Command::new("ditto")
        .arg(&src)
        .arg(&dest)
        .status()
        .context("could not run ditto")?;
    if !status.success() {
        bail!("file copy failed");
    }

    step(json_mode, "creating site + database");
    call(
        "site.register",
        json!({
            "name": new_name,
            "path": dest,
            "kind": "created",
            "php": php_version,
            "admin_user": site.get("admin_user").and_then(|v| v.as_str()),
        }),
    )
    .await?;
    let info = call("site.info", json!({"name": new_name})).await?;
    let new_site = info.get("site").cloned().context("bad site.info reply")?;

    // Dump source DB straight into the new DB (stream, no temp file).
    step(json_mode, "copying database");
    copy_database(&site, &new_site)?;

    // Point wp-config at the new DB.
    let new_db = new_site.get("db_name").and_then(|v| v.as_str()).unwrap();
    let new_user = new_site.get("db_user").and_then(|v| v.as_str()).unwrap();
    let new_pass = new_site.get("db_pass").and_then(|v| v.as_str()).unwrap();
    wp(
        &php_version,
        &dest,
        &[
            "config".into(),
            "set".into(),
            "DB_NAME".into(),
            new_db.into(),
        ],
    )?;
    wp(
        &php_version,
        &dest,
        &[
            "config".into(),
            "set".into(),
            "DB_USER".into(),
            new_user.into(),
        ],
    )?;
    wp(
        &php_version,
        &dest,
        &[
            "config".into(),
            "set".into(),
            "DB_PASSWORD".into(),
            new_pass.into(),
        ],
    )?;

    step(json_mode, "rewriting urls");
    let old_url = format!("https://{name}.localhost:{}", config.https_port);
    let new_url = format!("https://{new_name}.localhost:{}", config.https_port);
    wp(
        &php_version,
        &dest,
        &[
            "search-replace".into(),
            old_url.clone(),
            new_url.clone(),
            "--skip-columns=guid".into(),
        ],
    )?;
    let old_http = format!("http://{name}.localhost:{}", config.https_port);
    wp(
        &php_version,
        &dest,
        &[
            "search-replace".into(),
            old_http,
            new_url.clone(),
            "--skip-columns=guid".into(),
        ],
    )?;

    if json_mode {
        println!(
            "{}",
            json!({"cloned": name, "into": new_name, "url": new_url})
        );
    } else {
        println!("cloned {name} -> {new_name}\n  {new_url}");
    }
    Ok(0)
}

pub async fn creds(name: &str, json_mode: bool) -> Result<i32> {
    let info = call("site.info", json!({"name": name})).await?;
    let user = info
        .get("site")
        .and_then(|s| s.get("admin_user"))
        .and_then(|v| v.as_str())
        .unwrap_or("admin");
    let password = keychain::get_password(name)?.with_context(|| {
        format!("no stored password for `{name}` (only `ferro new` sites have them)")
    })?;
    if json_mode {
        println!(
            "{}",
            json!({"site": name, "admin_user": user, "admin_password": password})
        );
    } else {
        println!("site:      {name}");
        println!("admin user: {user}");
        println!("password:   {password}");
    }
    Ok(0)
}

// --- helpers -----------------------------------------------------------

fn copy_database(src_site: &serde_json::Value, dst_site: &serde_json::Value) -> Result<()> {
    let get = |site: &serde_json::Value, key: &str| {
        site.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let socket = format!("--socket={}", ferro_core::wpcli::mysql_socket().display());
    let mut dump = Command::new(ferro_core::paths::bin_dir().join("mysql/bin/mysqldump"))
        .arg(&socket)
        .arg(format!("-u{}", get(src_site, "db_user")))
        .arg(format!("-p{}", get(src_site, "db_pass")))
        .args(["--single-transaction", "--quick", "--set-gtid-purged=OFF"])
        .arg(get(src_site, "db_name"))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("could not run mysqldump")?;
    let dump_stdout = dump.stdout.take().context("mysqldump stdout missing")?;
    let mut mysql = Command::new(ferro_core::wpcli::mysql_client())
        .arg(&socket)
        .arg(format!("-u{}", get(dst_site, "db_user")))
        .arg(format!("-p{}", get(dst_site, "db_pass")))
        .arg(get(dst_site, "db_name"))
        .stdin(std::process::Stdio::from(dump_stdout))
        .spawn()
        .context("could not run mysql")?;
    let status = mysql.wait()?;
    let dump_status = dump.wait_with_output()?;
    if !status.success() || !dump_status.status.success() {
        bail!(
            "database copy failed: {}",
            String::from_utf8_lossy(&dump_status.stderr)
        );
    }
    Ok(())
}

fn wp(php_version: &str, path: &Path, args: &[String]) -> Result<()> {
    wp_stdin(php_version, path, args, None)
}

fn wp_stdin(php_version: &str, path: &Path, args: &[String], stdin: Option<&str>) -> Result<()> {
    let mut command = ferro_core::wpcli::wp_command(php_version, path, args);
    if stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    let mut child = command.spawn().context("could not run wp-cli")?;
    if let Some(input) = stdin {
        use std::io::Write as _;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(input.as_bytes());
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "wp {} failed:\n{}",
            args.first().cloned().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn step(json_mode: bool, message: &str) {
    if !json_mode {
        eprintln!("  .. {message}");
    }
}

fn title_case(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn wp_config_extra() -> String {
    let mut extra = String::from(
        "define('WP_ENVIRONMENT_TYPE', 'local');\n\
         define('WP_DEBUG', true);\n\
         define('WP_DEBUG_LOG', true);\n\
         define('WP_DEBUG_DISPLAY', false);\n",
    );
    // Mailpit constants are harmless before Mailpit exists; wp_mail uses
    // them via the ferro mail plugin (Phase 7).
    extra.push_str("define('FERRO', true);\n");
    extra
}

/// Resolve "latest"/"nightly"/pinned versions to a cached core zip,
/// downloading + checksum-verifying once per version/locale.
fn ensure_core(wp: &str, locale: &str) -> Result<PathBuf> {
    let cache = paths::app_data_dir().join("cache/wordpress");
    std::fs::create_dir_all(&cache)?;
    if wp == "nightly" {
        let zip = cache.join("nightly/wordpress.zip");
        if !zip.exists() {
            download(
                "https://wordpress.org/nightly-builds/wordpress-latest.zip",
                &zip,
                None,
            )?;
        }
        return Ok(zip);
    }
    let version = if wp == "latest" {
        latest_wp_version()?
    } else {
        wp.to_string()
    };
    let dir = cache.join(&version).join(locale);
    std::fs::create_dir_all(&dir)?;
    let zip = dir.join("wordpress.zip");
    if zip.exists() {
        return Ok(zip);
    }
    let (url, sha_url) = if locale == "en_US" {
        (
            format!("https://downloads.wordpress.org/release/wordpress-{version}.zip"),
            format!("https://downloads.wordpress.org/release/wordpress-{version}.zip.sha1"),
        )
    } else {
        (
            format!("https://downloads.wordpress.org/release/{locale}/wordpress-{version}.zip"),
            format!(
                "https://downloads.wordpress.org/release/{locale}/wordpress-{version}.zip.sha1"
            ),
        )
    };
    let expected = curl_string(&["-fsSL", &sha_url]).ok();
    download(&url, &zip, expected.as_deref())?;
    Ok(zip)
}

fn latest_wp_version() -> Result<String> {
    let json_text = curl_string(&["-fsSL", "https://api.wordpress.org/core/version-check/1.7/"])?;
    let parsed: serde_json::Value = serde_json::from_str(&json_text)?;
    parsed
        .get("offers")
        .and_then(|o| o.as_array())
        .and_then(|offers| offers.first())
        .and_then(|offer| offer.get("version"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .context("could not resolve the latest WordPress version")
}

fn download(url: &str, dest: &Path, expected_sha1: Option<&str>) -> Result<()> {
    let tmp = dest.with_extension("partial");
    let status = Command::new("curl")
        .args(["-fSL", "--retry", "3", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .context("could not run curl")?;
    if !status.success() {
        bail!("download failed: {url}");
    }
    if let Some(expected) = expected_sha1 {
        let actual = Command::new("shasum")
            .arg(&tmp)
            .output()
            .context("could not run shasum")?;
        let actual = String::from_utf8_lossy(&actual.stdout);
        let actual = actual.split_whitespace().next().unwrap_or("");
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            let _ = std::fs::remove_file(&tmp);
            bail!("checksum mismatch downloading {url} (expected {expected}, got {actual})");
        }
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

fn curl_string(args: &[&str]) -> Result<String> {
    let output = Command::new("curl")
        .args(args)
        .output()
        .context("could not run curl")?;
    if !output.status.success() {
        bail!("curl {:?} failed", args.last());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn extract_core(zip: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.join(".ferro-extract");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    let status = Command::new("unzip")
        .args(["-q"])
        .arg(zip)
        .arg("-d")
        .arg(&tmp)
        .status()
        .context("could not run unzip")?;
    if !status.success() {
        bail!("could not extract {}", zip.display());
    }
    let inner = tmp.join("wordpress");
    for entry in std::fs::read_dir(&inner)? {
        let entry = entry?;
        std::fs::rename(entry.path(), dest.join(entry.file_name()))?;
    }
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

// --- blueprints ---------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Blueprint {
    #[serde(default)]
    plugins: Vec<BlueprintPlugin>,
    #[serde(default)]
    themes: Vec<BlueprintPlugin>,
    #[serde(default)]
    options: std::collections::HashMap<String, String>,
    #[serde(default)]
    commands: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BlueprintPlugin {
    slug: String,
    #[serde(default)]
    activate: bool,
}

fn apply_blueprint(php_version: &str, path: &Path, file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("could not read blueprint {}", file.display()))?;
    let blueprint: Blueprint = toml::from_str(&text)
        .with_context(|| format!("blueprint {} is not valid TOML", file.display()))?;
    for plugin in &blueprint.plugins {
        let mut args = vec!["plugin".into(), "install".into(), plugin.slug.clone()];
        if plugin.activate {
            args.push("--activate".into());
        }
        wp(php_version, path, &args)?;
    }
    for theme in &blueprint.themes {
        let mut args = vec!["theme".into(), "install".into(), theme.slug.clone()];
        if theme.activate {
            args.push("--activate".into());
        }
        wp(php_version, path, &args)?;
    }
    for (name, value) in &blueprint.options {
        wp(
            php_version,
            path,
            &[
                "option".into(),
                "update".into(),
                name.clone(),
                value.clone(),
            ],
        )?;
    }
    for command in &blueprint.commands {
        let args: Vec<String> = command.split_whitespace().map(|s| s.to_string()).collect();
        if args.is_empty() {
            continue;
        }
        wp(php_version, path, &args)?;
    }
    Ok(())
}
