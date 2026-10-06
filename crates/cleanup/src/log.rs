//! The local record of every cleanup. It stays on this Mac and is never sent anywhere.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Category, Outcome};

pub const PUT_BACK_UNAVAILABLE: &str =
    "Put back is unavailable for this cleanup because the history could not be saved.";

impl LoggedItem {
    pub fn original_path(&self) -> PathBuf {
        PathBuf::from(std::ffi::OsString::from_vec(self.original.clone()))
    }

    pub fn trashed_path(&self) -> PathBuf {
        PathBuf::from(std::ffi::OsString::from_vec(self.trashed.clone()))
    }

    pub fn can_put_back(&self) -> bool {
        matches!(self.state, ItemState::Trashed | ItemState::RestoreFailed)
            && !self.trashed.is_empty()
    }

    fn capture(
        path: &Path,
        trashed: &Path,
        category: Category,
        size: u64,
        id: u64,
    ) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(trashed).ok()?;
        let parent = path.parent()?;
        let volume = std::fs::symlink_metadata(parent).ok()?.dev();
        Some(Self {
            id,
            original: path.as_os_str().as_bytes().to_vec(),
            trashed: trashed.as_os_str().as_bytes().to_vec(),
            volume,
            device: metadata.dev(),
            inode: metadata.ino(),
            directory: metadata.is_dir(),
            symlink: metadata.file_type().is_symlink(),
            category,
            size,
            state: ItemState::Trashed,
            failure: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    MovedToTrash,
    DeletedFromTrash,
    Restored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemState {
    Trashed,
    Restored,
    Deleted,
    RestoreFailed,
}

impl ItemState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trashed => "In the Trash",
            Self::Restored => "Put back",
            Self::Deleted => "Deleted",
            Self::RestoreFailed => "Put back failed",
        }
    }
}

/// One file or folder this app moved. Old log lines have no items, and those rows cannot be
/// put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggedItem {
    pub id: u64,
    /// Original path, as filesystem bytes.
    pub original: Vec<u8>,
    /// Trash path, as filesystem bytes.
    pub trashed: Vec<u8>,
    /// Device id of the original parent, so a different disk at the same path is refused.
    pub volume: u64,
    pub device: u64,
    pub inode: u64,
    pub directory: bool,
    pub symlink: bool,
    pub category: Category,
    pub size: u64,
    pub state: ItemState,
    #[serde(default)]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Seconds since 1970.
    pub time: u64,
    pub action: Action,
    pub category: Category,
    pub count: usize,
    pub bytes: u64,
    pub failed: usize,
    pub paths: Vec<String>,
    /// Per-item recovery records. Missing on logs written before put back existed.
    #[serde(default)]
    pub items: Vec<LoggedItem>,
}

impl LogEntry {
    /// One entry per category in `outcome`.
    pub fn from_outcome(outcome: &Outcome, time: SystemTime) -> Vec<Self> {
        let time = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut by_category: BTreeMap<Category, Self> = BTreeMap::new();
        let empty = |category| Self {
            time,
            action: Action::MovedToTrash,
            category,
            count: 0,
            bytes: 0,
            failed: 0,
            paths: Vec::new(),
            items: Vec::new(),
        };
        let mut next_id = time.saturating_mul(1_000_000);
        for moved in &outcome.moved {
            let entry = by_category
                .entry(moved.category)
                .or_insert_with(|| empty(moved.category));
            entry.count += 1;
            entry.bytes += moved.size;
            entry.paths.push(moved.path.to_string_lossy().into_owned());
            next_id += 1;
            if let Some(trashed) = &moved.trashed
                && let Some(item) =
                    LoggedItem::capture(&moved.path, trashed, moved.category, moved.size, next_id)
            {
                entry.items.push(item);
            }
        }
        for failed in &outcome.failed {
            by_category
                .entry(failed.category)
                .or_insert_with(|| empty(failed.category))
                .failed += 1;
        }
        by_category.into_values().collect()
    }
}

#[derive(Debug, Clone)]
pub struct OperationLog {
    path: PathBuf,
}

impl OperationLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn append(&self, entries: &[LogEntry]) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut lines = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut lines, entry)?;
            lines.push(b'\n');
        }
        file.write_all(&lines)
    }

    /// Replaces the log with `newest_first`, the same order [`Self::read`] returns.
    pub fn replace(&self, newest_first: &[LogEntry]) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        for entry in newest_first.iter().rev() {
            serde_json::to_writer(&mut file, entry)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        std::fs::rename(tmp, &self.path)
    }

    /// Newest first. Lines that can't be read are skipped.
    pub fn read(&self) -> io::Result<Vec<LogEntry>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut entries: Vec<LogEntry> = io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect();
        entries.reverse();
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Failed, Moved};

    #[test]
    fn groups_by_category_and_reads_back_newest_first() {
        let folder = tempfile::tempdir().unwrap();
        let log = OperationLog::new(folder.path().join("log/cleanup.jsonl"));
        let moved = |path: &str, category, size| Moved {
            path: PathBuf::from(path),
            trashed: None,
            node: None,
            category,
            size,
        };
        let outcome = Outcome {
            moved: vec![
                moved("/a", Category::Logs, 10),
                moved("/b", Category::Logs, 5),
                moved("/c", Category::OldInstallers, 100),
            ],
            failed: vec![Failed {
                path: PathBuf::from("/d"),
                node: None,
                category: Category::Logs,
                reason: "Permission denied".into(),
            }],
        };

        let first = LogEntry::from_outcome(&outcome, UNIX_EPOCH);
        log.append(&first).unwrap();
        let mut later = first[0].clone();
        later.time = 99;
        log.append(std::slice::from_ref(&later)).unwrap();

        assert_eq!(first.len(), 2);
        let logs = first
            .iter()
            .find(|entry| entry.category == Category::Logs)
            .unwrap();
        assert_eq!((logs.count, logs.bytes, logs.failed), (2, 15, 1));
        let read = log.read().unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].time, 99);
        assert!(read.iter().all(|entry| entry.items.is_empty()));
    }

    #[test]
    fn old_lines_without_items_still_load() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("cleanup.jsonl");
        std::fs::write(
            &path,
            r#"{"time":1,"action":"MovedToTrash","category":"Logs","count":1,"bytes":4,"failed":0,"paths":["/tmp/a"]}
"#,
        )
        .unwrap();
        let entries = OperationLog::new(path).read().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].items.is_empty());
        assert!(!entries[0].items.iter().any(LoggedItem::can_put_back));
    }
}
