use anyhow::{bail, Context, Result};
use ferro_core::ipc::call;
use serde_json::json;
use std::path::PathBuf;

/// `ferro wp <site> -- <args>`: run WP-CLI against a named site from
/// anywhere, passing through stdio and the exit code.
pub async fn wp(name: &str, args: Vec<String>) -> Result<i32> {
    let value = call("site.info", json!({"name": name})).await?;
    let site = value
        .get("site")
        .ok_or_else(|| anyhow::anyhow!("bad site.info reply"))?;
    let php_version = site
        .get("php_version")
        .and_then(|v| v.as_str())
        .unwrap_or("8.3");
    let root = PathBuf::from(
        site.get("path")
            .and_then(|v| v.as_str())
            .context("site has no path")?,
    );
    let phar = ferro_core::wpcli::wp_cli_phar();
    if !phar.exists() {
        bail!(
            "wp-cli.phar missing at {} — run scripts/fetch-wp-cli.sh",
            phar.display()
        );
    }
    let status = ferro_core::wpcli::wp_command(php_version, &root, &args)
        .current_dir(&root)
        .status()
        .context("could not start wp-cli")?;
    Ok(status.code().unwrap_or(1))
}
