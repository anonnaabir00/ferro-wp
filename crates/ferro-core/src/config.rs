use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Folders that are served automatically: <folder>/<name> -> <name>.localhost
    pub parked_paths: Vec<PathBuf>,
    pub default_php: String,
    pub http_port: u16,
    pub https_port: u16,
    pub mysql_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            parked_paths: vec![crate::paths::home_dir().join("Sites")],
            default_php: "8.3".into(),
            http_port: 8080,
            https_port: 8443,
            mysql_port: 3307,
        }
    }
}

impl Config {
    /// Load config.toml, writing defaults if it does not exist yet.
    pub fn load() -> Self {
        let path = crate::paths::config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!(
                        "warning: {} is invalid ({err}); using built-in defaults",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(_) => {
                let config = Self::default();
                if let Err(err) = config.save() {
                    eprintln!(
                        "warning: could not write default config to {}: {err}",
                        path.display()
                    );
                }
                config
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = crate::paths::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips_through_toml() {
        let config = Config {
            parked_paths: vec![PathBuf::from("/tmp/sites")],
            default_php: "8.4".into(),
            http_port: 9080,
            https_port: 9443,
            mysql_port: 33060,
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.http_port, 9080);
        assert_eq!(back.https_port, 9443);
        assert_eq!(back.mysql_port, 33060);
        assert_eq!(back.default_php, "8.4");
        assert_eq!(back.parked_paths, vec![PathBuf::from("/tmp/sites")]);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let back: Config = toml::from_str("http_port = 1234").unwrap();
        assert_eq!(back.http_port, 1234);
        assert_eq!(back.https_port, Config::default().https_port);
    }
}
