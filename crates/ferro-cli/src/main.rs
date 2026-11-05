mod daemon;
mod db;
mod doctor;
mod new;
mod pull;
mod replace;
mod sites;
mod update;
mod wp;

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
    /// Park a folder (default ~/Sites): every subfolder becomes a site.
    Park { path: Option<String> },
    /// List registered sites.
    Sites,
    /// Serve an arbitrary folder as <name>.localhost.
    Link { name: String, path: Option<String> },
    /// Remove a linked/parked site from the registry (keeps files and DB).
    Unlink { name: String },
    /// Open a site in the browser.
    Open {
        name: String,
        /// Open wp-admin instead.
        #[arg(long)]
        admin: bool,
    },
    /// Start a stopped site.
    Start { name: String },
    /// Stop a site (its pool is removed; the router returns 503).
    Stop { name: String },
    /// Restart a site, or all services (mysql, php-fpm) without a name.
    Restart { name: Option<String> },
    /// Tail a site's PHP log (or --mysql / --router).
    Logs {
        name: String,
        #[arg(long)]
        php: bool,
        #[arg(long)]
        mysql: bool,
        #[arg(long)]
        router: bool,
        /// Follow the log.
        #[arg(short, long)]
        follow: bool,
    },
    /// Trust the Ferro local CA in your login keychain (stops browser warnings).
    TrustCa,
    /// Run WP-CLI against a named site from anywhere.
    Wp {
        name: String,
        /// Arguments passed to wp-cli (use -- before flags).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Create a fresh WordPress site in seconds.
    New(Box<new::NewArgs>),
    /// Drop the database and move the site folder to the Trash.
    Destroy {
        name: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Copy files + database, rewrite URLs.
    Clone { name: String, new_name: String },
    /// Print the admin user + Keychain-stored password.
    Creds { name: String },
    /// Database tools for a site.
    Db {
        name: String,
        #[command(subcommand)]
        action: DbAction,
    },
    /// Search-replace in the site database (serialization-aware).
    Replace {
        name: String,
        from: String,
        to: String,
        /// rust (default, streaming) or wp-cli.
        #[arg(long, default_value = "rust")]
        engine: String,
    },
    /// Pull a production site over SSH: stream the DB (URLs replaced on
    /// the way), rsync files without uploads, proxy missing media.
    Pull(Box<pull::PullArgs>),
    /// Switch a site's PHP version.
    Php { name: String, version: String },
    /// Open the Mailpit mail catcher UI.
    Mail,
    /// Update bundled binaries and caches (default: everything).
    Update { components: Vec<String> },
}

#[derive(Subcommand)]
enum DbAction {
    /// Export the site database to a .sql file.
    Export { file: String },
    /// Import a .sql file into the site database.
    Import { file: String },
    /// Interactive mysql shell with the site's credentials.
    Shell,
    /// Open the site's database in Adminer (already logged in).
    Open,
    /// Print connection info for GUI clients (TablePlus etc.).
    Info {
        #[arg(long)]
        show_password: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Status => status(cli.json).await,
        Command::Doctor => doctor::run(cli.json).await,
        Command::Daemon { action } => daemon::run(action.clone(), cli.json),
        Command::Park { path } => sites::park(path.clone(), cli.json).await,
        Command::Sites => sites::sites(cli.json).await,
        Command::Link { name, path } => sites::link(name, path.clone(), cli.json).await,
        Command::Unlink { name } => sites::unlink(name, cli.json).await,
        Command::Open { name, admin } => sites::open(name, *admin, cli.json).await,
        Command::Start { name } => sites::set_running(name, true, cli.json).await,
        Command::Stop { name } => sites::set_running(name, false, cli.json).await,
        Command::Restart { name } => sites::restart(name.clone(), cli.json).await,
        Command::Logs {
            name,
            mysql,
            router,
            follow,
            ..
        } => sites::logs(
            name,
            if *mysql {
                "mysql"
            } else if *router {
                "router"
            } else {
                "php"
            },
            *follow,
        ),
        Command::TrustCa => sites::trust_ca(),
        Command::Wp { name, args } => wp::wp(name, args.clone()).await,
        Command::New(args) => new::new((*args.clone()).into(), cli.json).await,
        Command::Destroy { name, yes } => new::destroy(name, *yes, cli.json).await,
        Command::Clone { name, new_name } => new::clone_site(name, new_name, cli.json).await,
        Command::Creds { name } => new::creds(name, cli.json).await,
        Command::Db { name, action } => match action {
            DbAction::Export { file } => db::export(name, file).await,
            DbAction::Import { file } => db::import(name, file).await,
            DbAction::Shell => db::shell(name).await,
            DbAction::Open => db::open(name).await,
            DbAction::Info { show_password } => db::info(name, *show_password, cli.json).await,
        },
        Command::Pull(args) => pull::pull((**args).clone(), cli.json).await,
        Command::Php { name, version } => sites::set_php(name, version, cli.json).await,
        Command::Mail => sites::mail(),
        Command::Update { components } => update::update(components, cli.json).await,
        Command::Replace {
            name,
            from,
            to,
            engine,
        } => replace::replace(name, from, to, engine, cli.json).await,
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

    println!(
        "ferrod: running (PID {}, up {}), version {}",
        report.pid,
        humantime(report.uptime_secs),
        report.version
    );
    if report.router_running {
        println!(
            "router: http://<site>.localhost:{} -> https://<site>.localhost:{}",
            report.http_port, report.https_port
        );
    } else {
        println!("router: NOT running (see conflicts below)");
    }
    if report.services.is_empty() {
        println!("services: none");
    } else {
        println!("services:");
        for service in &report.services {
            let pid = service
                .pid
                .map(|p| format!("PID {p}"))
                .unwrap_or_else(|| "—".into());
            let extra = if service.restarts > 0 {
                format!(", {} restarts", service.restarts)
            } else {
                String::new()
            };
            let detail = service
                .detail
                .as_ref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            println!(
                "  {:<16} {:<9} {pid}{extra}{detail}",
                service.name, service.status
            );
        }
    }
    if report.conflicts.is_empty() {
        println!("conflicts: none");
    } else {
        println!("conflicts:");
        for c in &report.conflicts {
            println!("  ✗ :{} held by PID {} — {}", c.port, c.pid, c.command);
            println!("    fix: {}", c.fix);
        }
    }
    if report.sites.is_empty() {
        println!("sites: none yet");
    } else {
        println!("sites:");
        for site in &report.sites {
            let state = if site.running { "" } else { " (stopped)" };
            println!(
                "  {:<20} php {:<4} {url}{state}",
                site.name,
                site.php_version,
                url = site.url
            );
        }
    }
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
