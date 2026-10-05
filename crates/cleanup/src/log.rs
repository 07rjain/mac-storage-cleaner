//! The local record of every cleanup. It stays on this Mac and is never sent anywhere.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Category, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    MovedToTrash,
    DeletedFromTrash,
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
        };
        for moved in &outcome.moved {
            let entry = by_category
                .entry(moved.category)
                .or_insert_with(|| empty(moved.category));
            entry.count += 1;
            entry.bytes += moved.size;
            entry.paths.push(moved.path.to_string_lossy().into_owned());
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
    }
}
