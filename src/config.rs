use serde::Deserialize;
use std::fs;
use std::path::Path;

// Default value functions for serde — allows partial TOML config files.
fn default_cutoff() -> u8 {
    80
}
fn default_resume() -> u8 {
    70
}
fn default_debug() -> bool {
    false
}
fn default_log_file() -> String {
    "/data/adb/rsc/rsc.log".to_string()
}
fn default_log_max_size() -> u64 {
    512
}
fn default_log_keep() -> u32 {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_cutoff")]
    pub cutoff: u8,
    #[serde(default = "default_resume")]
    pub resume: u8,
    #[serde(default = "default_debug")]
    pub debug: bool,
    #[serde(default = "default_log_file")]
    pub log_file: String,
    #[serde(default = "default_log_max_size")]
    pub log_max_size_kb: u64,
    #[serde(default = "default_log_keep")]
    pub log_keep: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cutoff: 80,
            resume: 70,
            debug: false,
            log_file: "/data/adb/rsc/rsc.log".to_string(),
            log_max_size_kb: 512,
            log_keep: 3,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    InvalidRange(String),
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e)
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "io: {}", e),
            ConfigError::Parse(e) => write!(f, "parse: {}", e),
            ConfigError::InvalidRange(s) => write!(f, "range: {}", s),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Load config from the given path. If the path does not exist, returns
    /// the built-in default config (so the daemon can start on a fresh
    /// install before the user has written a config file).
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&content)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.cutoff <= self.resume {
            return Err(ConfigError::InvalidRange(format!(
                "cutoff ({}) must be strictly greater than resume ({})",
                self.cutoff, self.resume
            )));
        }
        if self.cutoff > 100 || self.resume > 100 {
            return Err(ConfigError::InvalidRange(
                "cutoff and resume must be in 0..=100".to_string(),
            ));
        }
        Ok(())
    }
}
