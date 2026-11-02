use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::process::Command;

#[derive(Subcommand)]
pub enum Action {
    /// Write the launchd agent plist (~/Library/LaunchAgents/dev.ferro.daemon.plist).
    Install,
    /// Load and start the agent (KeepAlive restarts it on crash).
    Start,
    /// Stop the agent and remove it from launchd.
    Stop,
    /// Stop then start.
    Restart,
}

const LABEL: &str = "dev.ferro.daemon";

pub fn run(action: Action, json: bool) -> Result<i32> {
    match action {
        Action::Install => install(json),
        Action::Start => start(json),
        Action::Stop => stop(json),
        Action::Restart => {
            let _ = bootout();
            start(json)
        }
    }
}

fn install(json: bool) -> Result<i32> {
    ferro_core::paths::ensure_dirs()?;
    let ferrod = ferro_core::paths::shims_dir().join("ferrod");
    if !ferrod.exists() {
        bail!(
            "ferrod binary not found at {}\n  fix: run scripts/install.sh from the ferro-wp repo first",
            ferrod.display()
        );
    }
    let plist_path = ferro_core::paths::launch_agent_path();
    let logs = ferro_core::paths::logs_dir();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>
"#,
        ferrod.display(),
        logs.join("ferrod.log").display(),
        logs.join("ferrod.err.log").display(),
    );
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("could not write {}", plist_path.display()))?;
    report(json, &format!("installed {}", plist_path.display()))
}

fn start(json: bool) -> Result<i32> {
    let plist = ferro_core::paths::launch_agent_path();
    if !plist.exists() {
        install(false)?;
    }
    let target = format!("gui/{}", uid());
    let boot = Command::new("launchctl")
        .args(["bootstrap", &target])
        .arg(&plist)
        .output()
        .context("failed to run launchctl bootstrap")?;
    if boot.status.success() {
        return report(json, "ferrod started (launchd will keep it alive)");
    }
    let stderr = String::from_utf8_lossy(&boot.stderr);
    if stderr.contains("already") || stderr.contains("Bootstrap failed: 5") {
        let kick = Command::new("launchctl")
            .args(["kickstart", "-k", &format!("{target}/{LABEL}")])
            .output()
            .context("failed to run launchctl kickstart")?;
        if kick.status.success() {
            return report(json, "ferrod restarted");
        }
        bail!(
            "launchctl kickstart failed: {}",
            String::from_utf8_lossy(&kick.stderr)
        );
    }
    bail!("launchctl bootstrap failed: {stderr}");
}

fn stop(json: bool) -> Result<i32> {
    match bootout()? {
        true => report(json, "ferrod stopped"),
        false => report(json, "ferrod was not running"),
    }
}

fn bootout() -> Result<bool> {
    let out = Command::new("launchctl")
        .args(["bootout", &format!("gui/{}/{LABEL}", uid())])
        .output()
        .context("failed to run launchctl bootout")?;
    Ok(out.status.success())
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string())
}

fn report(json: bool, message: &str) -> Result<i32> {
    if json {
        println!("{}", serde_json::json!({"ok": true, "message": message}));
    } else {
        println!("{message}");
    }
    Ok(0)
}
