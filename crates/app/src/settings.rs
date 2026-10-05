use std::io;
use std::path::PathBuf;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::APP_ID;

/// How the main window draws the current folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Chart {
    #[default]
    Sunburst,
    Treemap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub crash_reports: bool,
    /// Set once the welcome window has been completed.
    pub onboarded: bool,
    pub chart: Chart,
    /// Where these settings are saved; `None` keeps them in memory only.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            crash_reports: true,
            onboarded: false,
            chart: Chart::default(),
            path: None,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Missing or unreadable settings fall back to defaults.
    pub fn load() -> Self {
        let path = settings_path();
        let mut settings: Self = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        settings.path = path;
        settings
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }

    /// `false` in tests, where settings stay in memory and scans must not write last-scan.json
    /// into the real Application Support folder.
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }
}

/// Changes the app-wide settings, applies them, saves them and redraws every window.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut settings = cx.global::<Settings>().clone();
    change(&mut settings);
    telemetry::set_enabled(settings.crash_reports);
    if let Err(error) = settings.save() {
        tracing::warn!("failed to save settings: {error}");
    }
    cx.set_menus(crate::menus(&settings));
    cx.set_global(settings);
    cx.refresh_windows();
}

/// Where the app keeps its settings and the cleanup log.
pub fn data_dir() -> Option<PathBuf> {
    Some(
        std::env::home_dir()?
            .join("Library/Application Support")
            .join(APP_ID),
    )
}

pub fn cleanup_log_path() -> Option<PathBuf> {
    Some(data_dir()?.join("cleanup-log.jsonl"))
}

fn settings_path() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.json"))
}
