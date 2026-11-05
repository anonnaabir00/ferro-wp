//! `ferro update [component...]` — refresh bundled binaries and caches.
//! The fetch scripts are embedded into the binary so updates work long
//! after the repo checkout is gone.

use crate::new;
use anyhow::{Context, Result};
use serde_json::json;
use std::io::Write;
use std::process::Command;

const FETCH_PHP: &str = include_str!("../../../scripts/fetch-php.sh");
const FETCH_MYSQL: &str = include_str!("../../../scripts/fetch-mysql.sh");
const FETCH_WP_CLI: &str = include_str!("../../../scripts/fetch-wp-cli.sh");
const FETCH_COMPOSER: &str = include_str!("../../../scripts/fetch-composer.sh");
const FETCH_ADMINER: &str = include_str!("../../../scripts/fetch-adminer.sh");
const FETCH_MAILPIT: &str = include_str!("../../../scripts/fetch-mailpit.sh");

/// Adminer plugin files, materialized next to the embedded fetch script
/// (which finds them via FERRO_ASSETS_DIR).
const ADMINER_ASSETS: &[(&str, &str)] = &[
    ("index.php", include_str!("../../../assets/adminer/index.php")),
    (
        "ferro-bridge.php",
        include_str!("../../../assets/adminer/ferro-bridge.php"),
    ),
    (
        "ferro-plugin.php",
        include_str!("../../../assets/adminer/ferro-plugin.php"),
    ),
    (
        "adminer.css",
        include_str!("../../../assets/adminer/adminer.css"),
    ),
];

const COMPONENTS: &[&str] = &[
    "php",
    "mysql",
    "wp-cli",
    "composer",
    "adminer",
    "mailpit",
    "wordpress-cache",
];

pub async fn update(components: &[String], json_mode: bool) -> Result<i32> {
    let mut selected: Vec<String> = if components.is_empty() {
        COMPONENTS.iter().map(|c| c.to_string()).collect()
    } else {
        components.to_vec()
    };
    for component in &selected {
        if !COMPONENTS.contains(&component.as_str()) {
            anyhow::bail!(
                "unknown component `{component}` (known: {})",
                COMPONENTS.join(", ")
            );
        }
    }
    selected.dedup();

    let mut results = Vec::new();
    for component in &selected {
        let outcome = match component.as_str() {
            "php" => run_script("fetch-php.sh", FETCH_PHP, &[], &[]),
            "mysql" => run_script("fetch-mysql.sh", FETCH_MYSQL, &[], &[]),
            "wp-cli" => run_script("fetch-wp-cli.sh", FETCH_WP_CLI, &[], &[]),
            "composer" => run_script("fetch-composer.sh", FETCH_COMPOSER, &[], &[]),
            "adminer" => {
                let dir = std::env::temp_dir()
                    .join(format!("ferro-adminer-assets-{}", std::process::id()));
                let result = (|| -> Result<()> {
                    std::fs::create_dir_all(&dir)?;
                    for (name, content) in ADMINER_ASSETS {
                        std::fs::write(dir.join(name), content)?;
                    }
                    run_script(
                        "fetch-adminer.sh",
                        FETCH_ADMINER,
                        &[],
                        &[("FERRO_ASSETS_DIR", dir.display().to_string())],
                    )
                })();
                let _ = std::fs::remove_dir_all(&dir);
                result
            }
            "mailpit" => run_script("fetch-mailpit.sh", FETCH_MAILPIT, &[], &[]),
            "wordpress-cache" => new::ensure_core("latest", "en_US").map(|_| ()),
            other => unreachable!("validated above: {other}"),
        };
        match &outcome {
            Ok(()) => {
                eprintln!("  ✓ {component}");
                results.push(json!({"component": component, "ok": true}));
            }
            Err(err) => {
                eprintln!("  ✗ {component}: {err:#}");
                results.push(
                    json!({"component": component, "ok": false, "error": format!("{err:#}")}),
                );
            }
        }
    }

    let failed = results.iter().any(|r| r.get("ok") != Some(&json!(true)));
    // Restart services so new binaries take effect (the daemon
    // re-adopts everything on start).
    if !failed {
        let _ = Command::new(ferro_core::paths::shims_dir().join("ferro"))
            .args(["daemon", "restart"])
            .status();
    }
    if json_mode {
        println!("{}", json!({"updated": results}));
    }
    Ok(if failed { 1 } else { 0 })
}

fn run_script(name: &str, source: &str, args: &[&str], envs: &[(&str, String)]) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ferro-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let script = dir.join(name);
    {
        let mut file = std::fs::File::create(&script)?;
        file.write_all(source.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut command = Command::new("bash");
    command.arg(&script).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let status = command
        .status()
        .with_context(|| format!("could not run {name}"))?;
    let _ = std::fs::remove_dir_all(&dir);
    if !status.success() {
        anyhow::bail!("{name} failed ({status})");
    }
    Ok(())
}
