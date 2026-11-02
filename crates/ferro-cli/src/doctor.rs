use anyhow::Result;
use ferro_core::config::Config;
use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Level {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct Check {
    name: String,
    level: Level,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

pub async fn run(json: bool) -> Result<i32> {
    let config = Config::load();
    let mut checks = vec![
        check_daemon().await,
        check_port("http", config.http_port),
        check_port("https", config.https_port),
        check_port("mysql", config.mysql_port),
        check_path_shims(),
        check_launch_agent(),
        check_disk_space(),
    ];

    if json {
        let failed = checks.iter().any(|c| c.level == Level::Fail);
        println!("{}", serde_json::to_string_pretty(&checks)?);
        return Ok(if failed { 1 } else { 0 });
    }

    println!("ferro doctor\n");
    for check in &checks {
        let mark = match check.level {
            Level::Pass => "✓",
            Level::Warn => "!",
            Level::Fail => "✗",
        };
        println!("  {mark} {:<28} {}", check.name, check.detail);
        if let Some(fix) = &check.fix {
            println!("    {:<28} fix: {fix}", "");
        }
    }
    let failed = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warned = checks.iter().filter(|c| c.level == Level::Warn).count();
    println!();
    checks.shrink_to_fit();
    if failed > 0 {
        println!("{failed} problem(s) found that will break ferro.");
        Ok(1)
    } else if warned > 0 {
        println!("all critical checks passed; {warned} warning(s).");
        Ok(0)
    } else {
        println!("everything looks good.");
        Ok(0)
    }
}

async fn check_daemon() -> Check {
    match ferro_core::ipc::call("ping", serde_json::json!({})).await {
        Ok(value) => Check {
            name: "daemon".into(),
            level: Level::Pass,
            detail: format!(
                "ferrod is running (v{})",
                value.get("version").and_then(|v| v.as_str()).unwrap_or("?")
            ),
            fix: None,
        },
        Err(err) => Check {
            name: "daemon".into(),
            level: Level::Fail,
            detail: "ferrod is not reachable".into(),
            fix: Some(format!("{err}")),
        },
    }
}

fn check_port(name: &str, port: u16) -> Check {
    match ferro_core::ports::owner_of(port) {
        None => Check {
            name: format!("port :{port} ({name})"),
            level: Level::Pass,
            detail: "free".into(),
            fix: None,
        },
        Some(owner) if owner.is_ferro() => Check {
            name: format!("port :{port} ({name})"),
            level: Level::Pass,
            detail: format!("held by ferrod (PID {})", owner.pid),
            fix: None,
        },
        Some(owner) => Check {
            name: format!("port :{port} ({name})"),
            level: Level::Fail,
            detail: format!("held by {} (PID {})", owner.command, owner.pid),
            fix: Some(ferro_core::ports::suggest_fix(&owner, port)),
        },
    }
}

fn check_path_shims() -> Check {
    let shims = ferro_core::paths::shims_dir();
    let on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|entry| entry == shims.to_string_lossy());
    if on_path {
        Check {
            name: "PATH shims".into(),
            level: Level::Pass,
            detail: format!("{} is on PATH", shims.display()),
            fix: None,
        }
    } else {
        Check {
            name: "PATH shims".into(),
            level: Level::Warn,
            detail: format!("{} is not on PATH", shims.display()),
            fix: Some("add `export PATH=\"$HOME/.ferro/bin:$PATH\"` to ~/.zshrc".into()),
        }
    }
}

fn check_launch_agent() -> Check {
    let plist = ferro_core::paths::launch_agent_path();
    if !plist.exists() {
        return Check {
            name: "launchd agent".into(),
            level: Level::Warn,
            detail: format!("{} not installed", plist.display()),
            fix: Some("run `ferro daemon install && ferro daemon start`".into()),
        };
    }
    let loaded = Command::new("launchctl")
        .args(["list", "dev.ferro.daemon"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if loaded {
        Check {
            name: "launchd agent".into(),
            level: Level::Pass,
            detail: "installed and loaded".into(),
            fix: None,
        }
    } else {
        Check {
            name: "launchd agent".into(),
            level: Level::Warn,
            detail: "installed but not loaded".into(),
            fix: Some("run `ferro daemon start`".into()),
        }
    }
}

fn check_disk_space() -> Check {
    let home = ferro_core::paths::home_dir();
    let output = Command::new("df")
        .args(["-k", &home.to_string_lossy()])
        .output();
    let avail_kb = output.ok().and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines()
            .nth(1)?
            .split_whitespace()
            .nth(3)?
            .parse::<u64>()
            .ok()
    });
    match avail_kb {
        Some(kb) if kb >= 1_048_576 => Check {
            name: "disk space".into(),
            level: Level::Pass,
            detail: format!("{} GB free on home volume", kb / 1_048_576),
            fix: None,
        },
        Some(kb) => Check {
            name: "disk space".into(),
            level: Level::Fail,
            detail: format!("only {} MB free on home volume", kb / 1024),
            fix: Some("free up at least 1 GB — MySQL and PHP binaries need room".into()),
        },
        None => Check {
            name: "disk space".into(),
            level: Level::Warn,
            detail: "could not determine free space".into(),
            fix: None,
        },
    }
}
