//! Full Disk Access. macOS has no API to ask whether it was granted, so this checks whether
//! files that only Full Disk Access unlocks can be opened.

use std::fs::File;
use std::io::ErrorKind;

use gpui::App;

const SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

pub fn has_full_disk_access() -> bool {
    let Some(home) = std::env::home_dir() else {
        return false;
    };
    match File::open(home.join("Library/Application Support/com.apple.TCC/TCC.db")) {
        Ok(_) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            std::fs::read_dir(home.join("Library/Safari")).is_ok()
        }
        Err(_) => false,
    }
}

/// Opens System Settings › Privacy & Security › Full Disk Access.
pub fn open_settings(cx: &mut App) {
    cx.open_url(SETTINGS_URL);
}
