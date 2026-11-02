//! Integration test: the wp shim against a live ferro site.
//! Run with: cargo test -p ferro-cli --features integration
//! Requires: ferrod running and a WordPress site named `manual`
//! (e.g. ~/Sites/manual, or any site created with `ferro new`).
#![cfg(feature = "integration")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn shim() -> PathBuf {
    // CARGO_BIN_EXE_ferro is target/<profile>/ferro; the shim sits next to it.
    Path::new(env!("CARGO_BIN_EXE_ferro"))
        .parent()
        .unwrap()
        .join("ferro-shim")
}

fn manual_site() -> Option<PathBuf> {
    let path = ferro_core::paths::home_dir().join("Sites/manual");
    if path.join("wp-config.php").exists() {
        Some(path)
    } else {
        None
    }
}

fn run_shim(cwd: &Path, tool: &str, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(shim())
        .arg(tool)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("could not run shim");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn wp_option_get_siteurl() {
    let Some(site) = manual_site() else {
        eprintln!("skipping: ~/Sites/manual is not installed");
        return;
    };
    let (ok, stdout, stderr) = run_shim(&site, "wp", &["option", "get", "siteurl"]);
    assert!(ok, "wp option get siteurl failed: {stderr}");
    assert!(
        stdout.contains("manual.localhost"),
        "unexpected siteurl: {stdout}"
    );
}

#[test]
fn wp_plugin_list_json() {
    let Some(site) = manual_site() else {
        eprintln!("skipping: ~/Sites/manual is not installed");
        return;
    };
    let (ok, stdout, stderr) = run_shim(&site, "wp", &["plugin", "list", "--format=json"]);
    assert!(ok, "wp plugin list failed: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("plugin list is not JSON");
    assert!(parsed.as_array().map(|a| !a.is_empty()).unwrap_or(false));
}

#[test]
fn wp_db_query_select_1() {
    let Some(site) = manual_site() else {
        eprintln!("skipping: ~/Sites/manual is not installed");
        return;
    };
    let (ok, stdout, stderr) = run_shim(&site, "wp", &["db", "query", "SELECT 1"]);
    assert!(ok, "wp db query failed: {stderr}");
    assert!(stdout.trim().ends_with('1'), "unexpected output: {stdout}");
}

#[test]
fn outside_site_lists_known_sites() {
    let tmp = std::env::temp_dir();
    // A PATH with no real wp-cli on it: the shim must fail helpfully.
    let output = Command::new(shim())
        .arg("wp")
        .args(["core", "version"])
        .current_dir(&tmp)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", ferro_core::paths::shims_dir().display()),
        )
        .output()
        .expect("could not run shim");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "wp outside a site should fail");
    assert!(
        stderr.contains("not inside a known site") && stderr.contains("known sites:"),
        "unhelpful error: {stderr}"
    );
}
