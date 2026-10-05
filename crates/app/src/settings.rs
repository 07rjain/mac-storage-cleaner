use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::APP_ID;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub crash_reports: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            crash_reports: true,
        }
    }
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults.
    pub fn load() -> Self {
        settings_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = settings_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }
}

/// Where the app keeps its settings and the cleanup log.
pub fn data_dir() -> Option<PathBuf> {
    Some(
        std::env::home_dir()?
            .join("Library/Application Support")
            .join(APP_ID),
    )
}

fn settings_path() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.json"))
}
