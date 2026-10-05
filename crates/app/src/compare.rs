//! Folder sizes from the last finished scan, so the list can show what grew.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use scanner::{NodeFlags, NodeId, NodeKind, Tree};
use serde::{Deserialize, Serialize};

use crate::format;
use crate::settings;

/// Notes only when the change is at least 1 MB, so tiny files don't clutter the list.
const MIN_NOTE: u64 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub root: PathBuf,
    pub at: u64,
    pub root_size: u64,
    /// Relative POSIX path of each directory, other than the root, to its allocated size.
    pub dirs: HashMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    New,
    Delta(i64),
}

#[derive(Debug, Clone)]
pub struct Comparison {
    pub root_delta: i64,
    dirs: HashMap<String, Change>,
}

impl Snapshot {
    pub fn capture(tree: &Tree) -> Self {
        let root = tree.root_path().to_path_buf();
        let mut dirs = HashMap::new();
        for id in 0..tree.len() as NodeId {
            if tree.kind(id) != NodeKind::Directory
                || tree.flags(id).contains(NodeFlags::REMOVED)
                || id == tree.root()
            {
                continue;
            }
            if let Some(rel) = relative(tree, id) {
                dirs.insert(rel, tree.allocated(id));
            }
        }
        Self {
            root,
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            root_size: tree.allocated(tree.root()),
            dirs,
        }
    }

    pub fn compare(&self, now: &Snapshot) -> Option<Comparison> {
        if self.root != now.root {
            return None;
        }
        let mut dirs = HashMap::new();
        for (path, &size) in &now.dirs {
            match self.dirs.get(path) {
                None if size >= MIN_NOTE => {
                    dirs.insert(path.clone(), Change::New);
                }
                None => {}
                Some(&old) => {
                    dirs.insert(path.clone(), Change::Delta(size as i64 - old as i64));
                }
            }
        }
        Some(Comparison {
            root_delta: now.root_size as i64 - self.root_size as i64,
            dirs,
        })
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = file() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec(self)?)?;
        std::fs::rename(temp, path)
    }

    pub fn load_for(root: &Path) -> Option<Self> {
        let snapshot: Self = serde_json::from_slice(&std::fs::read(file()?).ok()?).ok()?;
        (snapshot.root == root).then_some(snapshot)
    }
}

fn file() -> Option<PathBuf> {
    Some(settings::data_dir()?.join("last-scan.json"))
}

impl Comparison {
    pub fn folder_note(&self, relative: &str) -> Option<String> {
        match self.dirs.get(relative)? {
            Change::New => Some("New".into()),
            Change::Delta(delta) if delta.abs() >= MIN_NOTE as i64 => Some(if *delta > 0 {
                format!("Grew {}", format::bytes(*delta as u64))
            } else {
                format!("Shrank {}", format::bytes(delta.unsigned_abs()))
            }),
            Change::Delta(_) => None,
        }
    }

    pub fn status_suffix(&self) -> Option<String> {
        let delta = self.root_delta;
        if delta.abs() < MIN_NOTE as i64 {
            return None;
        }
        Some(if delta > 0 {
            format!("Grew {} since last scan", format::bytes(delta as u64))
        } else {
            format!(
                "Shrank {} since last scan",
                format::bytes(delta.unsigned_abs())
            )
        })
    }
}

pub fn relative(tree: &Tree, id: NodeId) -> Option<String> {
    tree.path(id)
        .strip_prefix(tree.root_path())
        .ok()
        .filter(|rel| !rel.as_os_str().is_empty())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scanner::ScanOptions;
    use std::fs;
    use std::io::Write;

    fn write_bytes(path: &std::path::Path, length: usize) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&vec![0u8; length]).unwrap();
        file.sync_all().unwrap();
    }

    fn scan(root: &std::path::Path) -> Tree {
        scanner::scan(ScanOptions::new(root)).unwrap()
    }

    #[test]
    fn growing_and_new_folders_are_reported() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("keep")).unwrap();
        write_bytes(&root.path().join("keep/a.bin"), 2_000);
        let first = Snapshot::capture(&scan(root.path()));

        write_bytes(&root.path().join("keep/b.bin"), 2_000_000);
        fs::create_dir(root.path().join("fresh")).unwrap();
        write_bytes(&root.path().join("fresh/c.bin"), 2_000_000);
        let second = Snapshot::capture(&scan(root.path()));

        let cmp = first.compare(&second).expect("same root");
        assert!(cmp.root_delta > 1_000_000);
        assert_eq!(cmp.folder_note("fresh").as_deref(), Some("New"));
        let keep = cmp.folder_note("keep").expect("keep grew");
        assert!(keep.starts_with("Grew "), "{keep}");
        assert!(cmp.status_suffix().unwrap().contains("Grew"));
    }

    #[test]
    fn a_different_root_is_not_compared() {
        let a = Snapshot {
            root: PathBuf::from("/a"),
            at: 1,
            root_size: 10,
            dirs: HashMap::new(),
        };
        let b = Snapshot {
            root: PathBuf::from("/b"),
            at: 2,
            root_size: 20,
            dirs: HashMap::new(),
        };
        assert!(a.compare(&b).is_none());
    }

    #[test]
    fn small_changes_are_not_noted() {
        let mut dirs = HashMap::new();
        dirs.insert("tiny".into(), Change::Delta(500));
        let cmp = Comparison {
            root_delta: 500,
            dirs,
        };
        assert!(cmp.folder_note("tiny").is_none());
        assert!(cmp.status_suffix().is_none());
    }
}
