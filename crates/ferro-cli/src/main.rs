mod daemon;
mod doctor;

use anyhow::Result;
use clap::{Parser, Subcommand};
use ferro_core::ipc::StatusReport;

#[derive(Parser)]
#[command(
    name = "ferro",
    version,
    about = "Ferro WP — a fast, macOS-only local WordPress environment"
)]
struct Cli {
    /// Machine-readable output for scripting and the menu bar app.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Daemon, services, ports, conflicts.
    Status,
    /// Deep checks: port owners, PATH shims, launchd agent, disk space.
    Doctor,
    /// Manage the ferrod launchd agent.
    Daemon {
        #[command(subcommand)]
        action: daemon::Action,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Status => status(cli.json).await,
        Command::Doctor => doctor::run(cli.json).await,
        Command::Daemon { action } => daemon::run(action, cli.json),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("{err:#}");
            std::process::exit(1);
        }
    }
}

async fn status(json: bool) -> Result<i32> {
    let value = ferro_core::ipc::call("status", serde_json::json!({})).await?;
    let report: StatusReport = serde_json::from_value(value)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(if report.conflicts.is_empty() { 0 } else { 1 });
    }

    let up = humantime(report.uptime_secs);
    println!(
        "ferrod: running (PID {}, up {}), version {}",
        report.pid, up, report.version
    );
    if report.router_running {
        println!(
            "router: http://<site>.localhost:{} (running)",
            report.http_port
        );
    } else {
        println!("router: NOT running (see conflicts below)");
    }
    println!(
        "ports: http :{}, https :{}, mysql :{}",
        report.http_port, report.https_port, report.mysql_port
    );
    if report.conflicts.is_empty() {
        println!("conflicts: none");
    } else {
        println!("conflicts:");
        for c in &report.conflicts {
            println!("  ✗ :{} held by PID {} — {}", c.port, c.pid, c.command);
            println!("    fix: {}", c.fix);
        }
    }
    println!(
        "sites: {}",
        if report.sites.is_empty() {
            "none yet".into()
        } else {
            report.sites.join(", ")
        }
    );
    Ok(if report.conflicts.is_empty() { 0 } else { 1 })
}

fn humantime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}
