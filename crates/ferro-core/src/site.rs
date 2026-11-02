use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteKind {
    /// Found in a parked folder (e.g. ~/Sites/<name>).
    Parked,
    /// Arbitrary folder linked with `ferro link`.
    Linked,
    /// Made with `ferro new`.
    Created,
    /// Built-in service site (Adminer), never a WordPress folder.
    Builtin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProxy {
    /// Origin to pull missing uploads from, e.g. https://example.com
    pub origin: String,
    /// "redirect" (302 to origin) or "proxy" (fetch and stream).
    #[serde(default = "default_proxy_mode")]
    pub mode: String,
    /// Save proxied files locally on first request.
    #[serde(default)]
    pub cache: bool,
}

fn default_proxy_mode() -> String {
    "redirect".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub name: String,
    pub path: PathBuf,
    pub php_version: String,
    pub kind: SiteKind,
    #[serde(default = "default_true")]
    pub running: bool,
    #[serde(default)]
    pub xdebug: bool,
    #[serde(default)]
    pub db_name: Option<String>,
    #[serde(default)]
    pub db_user: Option<String>,
    #[serde(default)]
    pub db_pass: Option<String>,
    #[serde(default)]
    pub media_proxy: Option<MediaProxy>,
    /// Admin user created by `ferro new` (used by one-click admin login).
    #[serde(default)]
    pub admin_user: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Site {
    pub fn validate_name(name: &str) -> Result<(), String> {
        if name.is_empty() || name.len() > 63 {
            return Err("name must be 1-63 characters".to_string());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(
                "name may only contain lowercase letters, digits and hyphens (DNS-safe)"
                    .to_string(),
            );
        }
        if name.starts_with('-') || name.ends_with('-') {
            return Err("name may not start or end with a hyphen".to_string());
        }
        Ok(())
    }

    /// MySQL-safe database/user identifier for a site.
    /// Limited to 32 chars total (MySQL username limit).
    pub fn db_ident(name: &str) -> String {
        let mut ident: String = name
            .chars()
            .map(|c| if c == '-' { '_' } else { c })
            .collect();
        ident.truncate(26);
        format!("ferro_{ident}")
    }

    pub fn url(&self, https_port: u16) -> String {
        format!("https://{}.localhost:{}", self.name, https_port)
    }
}

/// The site registry, persisted to sites.json (0600 — it holds DB passwords).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    pub sites: Vec<Site>,
}

impl Registry {
    fn path() -> PathBuf {
        crate::paths::app_data_dir().join("sites.json")
    }

    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Site> {
        self.sites.iter().find(|s| s.name == name)
    }

    pub fn upsert(&mut self, site: Site) {
        match self.sites.iter_mut().find(|s| s.name == site.name) {
            Some(existing) => *existing = site,
            None => self.sites.push(site),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<Site> {
        let pos = self.sites.iter().position(|s| s.name == name)?;
        Some(self.sites.remove(pos))
    }

    pub fn by_host(&self, host: &str) -> Option<&Site> {
        // Exact match first, then strip leftmost labels so multisite
        // subdomain installs (sub.site.localhost) hit their network site.
        let mut name = host.strip_suffix(".localhost")?;
        loop {
            if let Some(site) = self.get(name) {
                return Some(site);
            }
            match name.split_once('.') {
                Some((_, rest)) => name = rest,
                None => return None,
            }
        }
    }

    /// Add any folders in `parked_paths` that are not registered yet.
    /// Returns the names of newly discovered sites.
    pub fn discover(&mut self, parked_paths: &[PathBuf], default_php: &str) -> Vec<String> {
        let mut added = Vec::new();
        for parked in parked_paths {
            let Ok(entries) = std::fs::read_dir(parked) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let name = name.to_lowercase();
                if name.starts_with('.') || Site::validate_name(&name).is_err() {
                    continue;
                }
                if self.get(&name).is_some() {
                    continue;
                }
                self.upsert(Site {
                    db_name: Some(Site::db_ident(&name)),
                    db_user: Some(Site::db_ident(&name)),
                    db_pass: Some(crate::random_token(24)),
                    name: name.clone(),
                    path,
                    php_version: default_php.to_string(),
                    kind: SiteKind::Parked,
                    running: true,
                    xdebug: false,
                    media_proxy: None,
                    admin_user: None,
                });
                added.push(name);
            }
        }
        added
    }

    /// Walk up from `dir` to find the site that contains it.
    pub fn find_by_path(&self, dir: &Path) -> Option<&Site> {
        let mut best: Option<&Site> = None;
        for site in &self.sites {
            if dir.starts_with(&site.path) {
                match best {
                    Some(b) if b.path.as_os_str().len() >= site.path.as_os_str().len() => {}
                    _ => best = Some(site),
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_dns_safe_names() {
        assert!(Site::validate_name("demo").is_ok());
        assert!(Site::validate_name("my-site-2").is_ok());
        assert!(Site::validate_name("Demo").is_err());
        assert!(Site::validate_name("my_site").is_err());
        assert!(Site::validate_name("-bad").is_err());
        assert!(Site::validate_name("").is_err());
    }

    #[test]
    fn db_ident_is_mysql_safe() {
        assert_eq!(Site::db_ident("my-site"), "ferro_my_site");
        assert!(Site::db_ident(&"a".repeat(100)).len() <= 32);
    }

    #[test]
    fn finds_site_by_nested_path() {
        let mut registry = Registry::default();
        registry.upsert(Site {
            name: "demo".into(),
            path: PathBuf::from("/Users/x/Sites/demo"),
            php_version: "8.3".into(),
            kind: SiteKind::Parked,
            running: true,
            xdebug: false,
            db_name: None,
            db_user: None,
            db_pass: None,
            media_proxy: None,
            admin_user: None,
        });
        assert!(registry
            .find_by_path(Path::new("/Users/x/Sites/demo/wp-content/themes"))
            .is_some());
        assert!(registry.find_by_path(Path::new("/elsewhere")).is_none());
    }
}
