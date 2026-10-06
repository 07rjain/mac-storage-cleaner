//! Puts back one item this app moved to the Trash. It does not restore anything else, and it
//! does not overwrite a file that is already at the original path.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSString, NSURL};

use crate::log::{ItemState, LoggedItem};
use crate::trash::is_in_trash;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreFailure {
    Gone,
    Changed,
    Occupied,
    ParentMissing,
    VolumeChanged,
    Permission,
    NoRecord,
}

impl RestoreFailure {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Gone => "Gone from the Trash",
            Self::Changed => "Identity changed",
            Self::Occupied => "Destination occupied",
            Self::ParentMissing => "Parent missing",
            Self::VolumeChanged => "Volume changed",
            Self::Permission => "Permission",
            Self::NoRecord => "No recovery record",
        }
    }
}

/// Moves `item` from its Trash path back to the original path.
pub fn put_back(item: &LoggedItem) -> Result<(), RestoreFailure> {
    let Some(home) = std::env::home_dir() else {
        return Err(RestoreFailure::NoRecord);
    };
    put_back_at(item, &home)
}

pub fn put_back_at(item: &LoggedItem, home: &Path) -> Result<(), RestoreFailure> {
    if item.trashed.is_empty() || item.original.is_empty() {
        return Err(RestoreFailure::NoRecord);
    }
    let source = path_from(&item.trashed);
    let destination = path_from(&item.original);
    if !is_in_trash(&source, home) {
        return Err(RestoreFailure::NoRecord);
    }
    let metadata = match std::fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RestoreFailure::Gone);
        }
        Err(_) => return Err(RestoreFailure::Permission),
    };
    if metadata.dev() != item.device
        || metadata.ino() != item.inode
        || metadata.is_dir() != item.directory
        || metadata.file_type().is_symlink() != item.symlink
    {
        return Err(RestoreFailure::Changed);
    }
    let Some(parent) = destination.parent() else {
        return Err(RestoreFailure::ParentMissing);
    };
    if parent.as_os_str().is_empty() {
        return Err(RestoreFailure::ParentMissing);
    }
    let parent_meta = match std::fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RestoreFailure::ParentMissing);
        }
        Err(_) => return Err(RestoreFailure::Permission),
    };
    if parent_meta.dev() != item.volume {
        return Err(RestoreFailure::VolumeChanged);
    }
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(RestoreFailure::Occupied);
    }
    move_item(&source, &destination)
}

/// Restores every item that can be put back. One failure does not cancel the others.
/// Returns how many were restored.
pub fn put_back_items(items: &mut [LoggedItem], home: &Path) -> usize {
    let mut restored = 0;
    for item in items.iter_mut().filter(|item| item.can_put_back()) {
        match put_back_at(item, home) {
            Ok(()) => {
                item.state = ItemState::Restored;
                item.failure = None;
                restored += 1;
            }
            Err(error) => {
                item.state = ItemState::RestoreFailed;
                item.failure = Some(error.reason().to_string());
            }
        }
    }
    restored
}

fn path_from(bytes: &[u8]) -> std::path::PathBuf {
    std::path::PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

fn move_item(source: &Path, destination: &Path) -> Result<(), RestoreFailure> {
    let source = source
        .to_str()
        .ok_or(RestoreFailure::Permission)?
        .to_string();
    let destination = destination
        .to_str()
        .ok_or(RestoreFailure::Permission)?
        .to_string();
    autoreleasepool(|_| {
        let from = NSURL::fileURLWithPath(&NSString::from_str(&source));
        let to = NSURL::fileURLWithPath(&NSString::from_str(&destination));
        match NSFileManager::defaultManager().moveItemAtURL_toURL_error(&from, &to) {
            Ok(()) => Ok(()),
            Err(error) => {
                let domain = error.domain().to_string();
                let code = error.code();
                Err(match (domain.as_str(), code) {
                    ("NSCocoaErrorDomain", 516) => RestoreFailure::Occupied,
                    ("NSCocoaErrorDomain", 4) => RestoreFailure::Gone,
                    ("NSCocoaErrorDomain", 513) => RestoreFailure::Permission,
                    _ => RestoreFailure::Permission,
                })
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Category;
    use crate::log::{ItemState, LoggedItem};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn trashed_item(home: &Path, name: &str, body: &[u8], occupy: bool) -> LoggedItem {
        let parent = home.join("code");
        std::fs::create_dir_all(&parent).unwrap();
        let original = parent.join(name);
        if occupy {
            std::fs::write(&original, b"already here").unwrap();
        }
        let trash = home.join(".Trash");
        std::fs::create_dir_all(&trash).unwrap();
        let trashed = trash.join(name);
        std::fs::write(&trashed, body).unwrap();
        let metadata = std::fs::symlink_metadata(&trashed).unwrap();
        let volume = std::fs::symlink_metadata(&parent).unwrap().dev();
        LoggedItem {
            id: 1,
            original: original.as_os_str().as_bytes().to_vec(),
            trashed: trashed.as_os_str().as_bytes().to_vec(),
            volume,
            device: metadata.dev(),
            inode: metadata.ino(),
            directory: false,
            symlink: false,
            category: Category::Logs,
            size: body.len() as u64,
            state: ItemState::Trashed,
            failure: None,
        }
    }

    #[test]
    fn puts_an_item_back_and_leaves_an_occupied_path_alone() {
        let home = tempfile::tempdir().unwrap();
        let mut items = vec![
            trashed_item(home.path(), "back.txt", b"restored", false),
            trashed_item(home.path(), "kept.txt", b"from trash", true),
        ];
        let restored = put_back_items(&mut items, home.path());
        assert_eq!(
            restored,
            1,
            "{:?}",
            items
                .iter()
                .map(|item| item.failure.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            std::fs::read(home.path().join("code/back.txt")).unwrap(),
            b"restored"
        );
        assert!(!home.path().join(".Trash/back.txt").exists());
        assert_eq!(items[0].state, ItemState::Restored);
        assert_eq!(
            std::fs::read(home.path().join("code/kept.txt")).unwrap(),
            b"already here"
        );
        assert!(home.path().join(".Trash/kept.txt").exists());
        assert_eq!(items[1].state, ItemState::RestoreFailed);
        assert_eq!(
            items[1].failure.as_deref(),
            Some(RestoreFailure::Occupied.reason())
        );
        items[1].state = ItemState::Trashed;
        items[1].failure = None;
        assert_eq!(put_back_items(&mut items, home.path()), 0);
        assert!(
            items.iter().all(|item| item.state != ItemState::Trashed),
            "a restored item is not left for Empty Trash"
        );
    }

    #[test]
    fn refuses_a_changed_trash_item() {
        let home = tempfile::tempdir().unwrap();
        let mut item = trashed_item(home.path(), "note.txt", b"one", false);
        std::fs::remove_file(item.trashed_path()).unwrap();
        std::fs::write(item.trashed_path(), b"two").unwrap();
        assert_eq!(
            put_back_at(&item, home.path()),
            Err(RestoreFailure::Changed)
        );
        item.state = ItemState::Restored;
        assert!(!item.can_put_back());
    }
}
