//! SSH pull support: run commands on a remote WordPress host, parse its
//! wp-config.php, stream a database dump, and rsync the files. SSH is
//! shelled out to (never reimplemented) so the user's ~/.ssh/config,
//! agent and host aliases all apply.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Directories under wp-content that are caches or backups — excluded
/// from pulls unless --include-uploads is given (uploads stay remote
/// and are served through the media proxy).
pub const RSYNC_EXCLUDES: &[&str] = &[
    "/wp-content/uploads/",
    "/wp-content/cache/",
    "/wp-content/backups/",
    "/wp-content/backup/",
    "/wp-content/ai1wm-backups/",
    "/wp-content/updraft/",
    "/wp-content/backupwordpress/",
    "/wp-content/backups-dup-pro/",
    "/wp-content/backup-guard/",
    "/wp-content/et-cache/",
    "/wp-content/w3tc-config/",
    "/wp-content/nfwlog/",
    "/node_modules/",
];

/// Include uploads too (used with --include-uploads): only caches and
/// backups are excluded.
pub const RSYNC_EXCLUDES_WITH_UPLOADS: &[&str] = &[
    "/wp-content/cache/",
    "/wp-content/backups/",
    "/wp-content/backup/",
    "/wp-content/ai1wm-backups/",
    "/wp-content/updraft/",
    "/wp-content/backupwordpress/",
    "/wp-content/backups-dup-pro/",
    "/wp-content/backup-guard/",
    "/wp-content/et-cache/",
    "/node_modules/",
];

/// Build an ssh command for the target, respecting ~/.ssh/config and
/// the agent. FERRO_SSH_BIN overrides the ssh binary (used by the
/// end-to-end test, which substitutes a local executor).
pub fn ssh_command(target: &str, remote_command: &str) -> Command {
    let ssh = std::env::var("FERRO_SSH_BIN").unwrap_or_else(|_| "ssh".to_string());
    let mut command = Command::new(&ssh);
    if ssh.ends_with("ssh") {
        // BatchMode: never prompt interactively (the CLI surfaces the
        // error instead of hanging on a password question).
        command.arg("-o").arg("BatchMode=yes");
    }
    command.arg(target).arg(remote_command);
    command
}

/// Run a remote command and capture stdout.
pub fn ssh_capture(target: &str, remote_command: &str) -> Result<String> {
    let output = ssh_command(target, remote_command)
        .stderr(Stdio::piped())
        .output()
        .context("could not run ssh")?;
    if !output.status.success() {
        bail!(
            "ssh {} failed ({}): {}",
            target,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Connection details parsed from a remote wp-config.php.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWpConfig {
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    /// As written in wp-config: "host", "host:port", "host:/path.sock".
    pub db_host: String,
    pub table_prefix: String,
}

impl RemoteWpConfig {
    pub fn parse(wp_config: &str) -> Result<Self> {
        let constant = |name: &str| -> Result<String> {
            parse_define(wp_config, name)
                .ok_or_else(|| anyhow::anyhow!("wp-config.php has no {name} definition"))
        };
        let table_prefix = parse_var(wp_config, "table_prefix").unwrap_or_else(|| "wp_".into());
        Ok(Self {
            db_name: constant("DB_NAME")?,
            db_user: constant("DB_USER")?,
            db_password: constant("DB_PASSWORD")?,
            db_host: constant("DB_HOST")?,
            table_prefix,
        })
    }

    /// mysqldump/mysql arguments for this connection.
    pub fn mysql_args(&self, bin: &str) -> Vec<String> {
        let (host, port, socket) = split_db_host(&self.db_host);
        let mut args = Vec::new();
        match (socket, port) {
            (Some(sock), _) => args.push(format!("--socket={sock}")),
            (None, Some(port)) => {
                args.push(format!("--host={host}"));
                args.push(format!("--port={port}"));
            }
            (None, None) => args.push(format!("--host={host}")),
        }
        args.push(format!("--user={}", self.db_user));
        args.push(format!("--password={}", self.db_password));
        if bin == "mysqldump" {
            args.extend(
                [
                    "--single-transaction",
                    "--quick",
                    "--set-gtid-purged=OFF",
                    "--no-tablespaces",
                ]
                .map(String::from),
            );
        }
        args.push(self.db_name.clone());
        args
    }
}

/// Split a WP DB_HOST into (host, port, socket).
/// "localhost" / "host:3306" / "host:3307" / "localhost:/var/mysql.sock".
fn split_db_host(db_host: &str) -> (String, Option<u16>, Option<String>) {
    if let Some((host, rest)) = db_host.rsplit_once(':') {
        if rest.starts_with('/') {
            return (host.to_string(), None, Some(rest.to_string()));
        }
        if let Ok(port) = rest.parse::<u16>() {
            return (host.to_string(), Some(port), None);
        }
    }
    (db_host.to_string(), None, None)
}

/// Parse `define('NAME', 'value');` — single or double quotes, any
/// number of defines per line.
fn parse_define(text: &str, name: &str) -> Option<String> {
    let mut rest = text;
    while let Some(pos) = rest.find("define") {
        rest = &rest[pos + "define".len()..];
        let Some(after_paren) = rest.trim_start().strip_prefix('(') else {
            continue;
        };
        let after_paren = after_paren.trim_start();
        let Some((key, key_len)) = parse_quoted(after_paren) else {
            continue;
        };
        if key != name {
            continue;
        }
        let Some(after_comma) = after_paren[key_len..].trim_start().strip_prefix(',') else {
            continue;
        };
        let (value, _) = parse_quoted(after_comma.trim_start())?;
        return Some(value);
    }
    None
}

/// Parse a quoted PHP string at the start of `s`; returns the
/// unescaped value and the consumed byte length (including quotes).
fn parse_quoted(s: &str) -> Option<(String, usize)> {
    let quote = s.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let mut out = String::new();
    let mut len = quote.len_utf8();
    let mut escaped = false;
    for c in s[len..].chars() {
        len += c.len_utf8();
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some((out, len));
        } else {
            out.push(c);
        }
    }
    None
}

/// Parse `$name = 'value';`
fn parse_var(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(&format!("${name}")) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let unquoted = rest.trim().trim_end_matches(';').trim_matches(['\'', '"']);
        return Some(unescape_php_string(unquoted));
    }
    None
}

fn unescape_php_string(value: &str) -> String {
    value
        .replace("\\'", "'")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

/// The decompressor to use on the local side of the ssh stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Zstd,
    Gzip,
    None,
}

impl Compression {
    /// Remote pipeline suffix and local decompressor binary.
    pub fn remote_pipe(&self) -> &'static str {
        match self {
            Compression::Zstd => " | zstd -c",
            Compression::Gzip => " | gzip -c",
            Compression::None => "",
        }
    }

    pub fn local_binary(&self) -> Option<&'static str> {
        match self {
            Compression::Zstd => Some("zstd"),
            Compression::Gzip => Some("gzip"),
            Compression::None => None,
        }
    }

    /// Pick the best transport available on both ends.
    pub fn negotiate(target: &str) -> Self {
        let remote = ssh_capture(target, "command -v zstd; command -v gzip").unwrap_or_default();
        let local_zstd = Command::new("zstd")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if remote.contains("zstd") && local_zstd {
            Compression::Zstd
        } else if remote.contains("gzip") {
            Compression::Gzip
        } else {
            Compression::None
        }
    }
}

/// rsync the remote WordPress root into `local_root`, excluding caches
/// and backups (and uploads, unless `--include-uploads`).
pub fn rsync_files(
    target: &str,
    remote_root: &Path,
    local_root: &Path,
    include_uploads: bool,
) -> Result<()> {
    let excludes: &[&str] = if include_uploads {
        RSYNC_EXCLUDES_WITH_UPLOADS
    } else {
        RSYNC_EXCLUDES
    };
    let remote = format!(
        "{}:{}/",
        target,
        remote_root.display().to_string().trim_end_matches('/')
    );
    let local = format!("{}/", local_root.display());
    let mut args: Vec<String> = vec!["-az".into(), "--delete-excluded".into()];
    // --info=progress2 needs rsync 3.1+; openrsync (macOS default)
    // supports neither that nor --progress reliably, so only add a
    // progress flag when GNU rsync is present.
    let version = Command::new("rsync")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if version.contains("rsync  version 3") || version.contains("rsync  version 2") {
        let flag = if version.contains("version 3.1")
            || version.contains("version 3.2")
            || version.contains("version 3.3")
            || version.contains("version 3.4")
        {
            "--info=progress2"
        } else {
            "--progress"
        };
        args.push(flag.into());
    }
    for exclude in excludes {
        args.push("--exclude".into());
        args.push((*exclude).into());
    }
    args.push(remote);
    args.push(local);
    let status = Command::new("rsync")
        .args(&args)
        .status()
        .context("could not run rsync")?;
    if !status.success() {
        bail!("rsync failed ({status}) — is the remote path a WordPress root?");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WP_CONFIG: &str = r#"<?php
define( 'DB_NAME', 'prod_db' );
define( 'DB_USER', "prod_user" );
define( 'DB_PASSWORD', 's3cr\'et' );
define( 'DB_HOST', 'localhost:/var/run/mysql.sock' );
$table_prefix = 'wp_';
"#;

    #[test]
    fn parses_wp_config() {
        let config = RemoteWpConfig::parse(WP_CONFIG).unwrap();
        assert_eq!(config.db_name, "prod_db");
        assert_eq!(config.db_user, "prod_user");
        assert_eq!(config.db_password, "s3cr'et");
        assert_eq!(config.db_host, "localhost:/var/run/mysql.sock");
        assert_eq!(config.table_prefix, "wp_");
    }

    #[test]
    fn parses_custom_prefix_and_tcp_host() {
        let text = "define('DB_NAME','d');define('DB_USER','u');define('DB_PASSWORD','p');\ndefine('DB_HOST','db.internal:3307');\n$table_prefix = 'site2_';";
        let config = RemoteWpConfig::parse(text).unwrap();
        assert_eq!(config.table_prefix, "site2_");
        let args = config.mysql_args("mysqldump");
        assert!(args.contains(&"--host=db.internal".to_string()));
        assert!(args.contains(&"--port=3307".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--socket")));
    }

    #[test]
    fn socket_hosts_become_socket_args() {
        let config = RemoteWpConfig::parse(WP_CONFIG).unwrap();
        let args = config.mysql_args("mysql");
        assert!(args.contains(&"--socket=/var/run/mysql.sock".to_string()));
        assert!(args.last().is_some_and(|a| a == "prod_db"));
    }

    #[test]
    fn split_db_host_variants() {
        assert_eq!(
            split_db_host("localhost"),
            ("localhost".to_string(), None, None)
        );
        assert_eq!(
            split_db_host("127.0.0.1:3307"),
            ("127.0.0.1".to_string(), Some(3307), None)
        );
        assert_eq!(
            split_db_host("localhost:/tmp/mysql.sock"),
            (
                "localhost".to_string(),
                None,
                Some("/tmp/mysql.sock".to_string())
            )
        );
    }
}
