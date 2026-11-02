use serde::{Deserialize, Serialize};
use std::process::Command;

/// The process currently listening on a TCP port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortOwner {
    pub pid: u32,
    pub command: String,
    pub full_command: String,
    pub user: String,
}

impl PortOwner {
    pub fn is_ferro(&self) -> bool {
        self.command == "ferrod" || self.full_command.contains("/ferrod")
    }
}

/// Who is listening on `port`? Uses lsof; returns None when the port is free.
pub fn owner_of(port: u16) -> Option<PortOwner> {
    let output = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-F", "pcu"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (pid, command, user) = parse_lsof(&stdout)?;
    let full_command = full_command_of(pid).unwrap_or_else(|| command.clone());
    Some(PortOwner {
        pid,
        command,
        full_command,
        user,
    })
}

/// Parse lsof -F pcu output: lines like `p642`, `cnginx`, `uanonnaabir`.
fn parse_lsof(output: &str) -> Option<(u32, String, String)> {
    let mut pid = None;
    let mut command = None;
    let mut user = None;
    for line in output.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = value.parse::<u32>().ok(),
            "c" => command = Some(value.to_string()),
            "u" => user = Some(value.to_string()),
            _ => {}
        }
        if let (Some(p), Some(c), Some(u)) = (pid, command.clone(), user.clone()) {
            return Some((p, c, u));
        }
    }
    match (pid, command, user) {
        (Some(p), Some(c), Some(u)) => Some((p, c, u)),
        _ => None,
    }
}

fn full_command_of(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    let cmd = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
}

/// A concrete, user-facing fix suggestion for a port conflict.
pub fn suggest_fix(owner: &PortOwner, port: u16) -> String {
    let cmd = owner.command.to_lowercase();
    let full = owner.full_command.to_lowercase();
    let pid = owner.pid;
    if cmd.contains("nginx") && full.contains("herd") {
        return format!(
            "Laravel Herd's nginx (PID {pid}) is holding :{port} — quit Herd or move Ferro's ports in ~/Library/Application Support/ferro/config.toml"
        );
    }
    if cmd.contains("nginx") {
        return format!(
            "nginx (PID {pid}) is holding :{port} — if it came from Homebrew run `sudo brew services stop nginx`; Valet: `valet stop`; otherwise `kill {pid}`"
        );
    }
    if cmd.contains("httpd") || cmd.contains("apache") {
        return format!(
            "Apache (PID {pid}) is holding :{port} — run `sudo apachectl stop` (check MAMP if installed)"
        );
    }
    if cmd.contains("mysqld") || cmd.contains("mariadbd") {
        return format!(
            "{} (PID {pid}) is holding :{port} — if it came from Homebrew run `brew services stop mysql`, or change mysql_port in ~/Library/Application Support/ferro/config.toml",
            owner.command
        );
    }
    if cmd.contains("php-fpm") || cmd.contains("php") {
        return format!(
            "php-fpm (PID {pid}) is holding :{port} — if it came from Homebrew run `brew services stop php`, or `kill {pid}`"
        );
    }
    if cmd.contains("valet") || full.contains("valet") {
        return format!(
            "Laravel Valet (PID {pid}) is holding :{port} — run `valet stop` or `valet uninstall`"
        );
    }
    if cmd.contains("python") {
        return format!(
            "{} (PID {pid}) is holding :{port} — stop it with `kill {pid}`",
            owner.command
        );
    }
    if cmd.contains("node") || cmd.contains("caddy") {
        return format!(
            "{} (PID {pid}) is holding :{port} — stop it with `kill {pid}`",
            owner.command
        );
    }
    format!(
        "{} (PID {pid}) is holding :{port} — stop it with `kill {pid}`, or change the port in ~/Library/Application Support/ferro/config.toml",
        owner.full_command
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsof_machine_output() {
        let sample = "p642\ncnginx\nuanonnaabir\np643\ncnginx\nuanonnaabir\n";
        let (pid, command, user) = parse_lsof(sample).unwrap();
        assert_eq!(pid, 642);
        assert_eq!(command, "nginx");
        assert_eq!(user, "anonnaabir");
    }

    #[test]
    fn returns_none_for_empty_output() {
        assert!(parse_lsof("").is_none());
    }

    #[test]
    fn fix_suggestion_names_pid_and_port() {
        let owner = PortOwner {
            pid: 588,
            command: "nginx".into(),
            full_command: "nginx: master process".into(),
            user: "root".into(),
        };
        let fix = suggest_fix(&owner, 8080);
        assert!(fix.contains("588"));
        assert!(fix.contains("8080"));
        assert!(fix.contains("brew services stop nginx"));
    }

    #[test]
    fn herd_nginx_gets_herd_specific_fix() {
        let owner = PortOwner {
            pid: 42,
            command: "nginx".into(),
            full_command:
                "nginx: master process /Users/x/Library/Application Support/Herd/bin/nginx".into(),
            user: "x".into(),
        };
        assert!(suggest_fix(&owner, 443).contains("Herd"));
    }
}
