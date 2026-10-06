//! Moving basket items to the Trash, after checking each one again.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2_foundation::{NSFileManager, NSString, NSURL};
use scanner::NodeId;

use crate::basket::{BasketItem, Identity};
use crate::safety::{self, Refusal};
use crate::{Category, Inventory, LeftoverProof, Places, RunningApps};

#[derive(Debug, Clone)]
pub struct Moved {
    pub path: PathBuf,
    /// Where the item is now, if the Trash reported it.
    pub trashed: Option<PathBuf>,
    pub node: Option<NodeId>,
    pub category: Category,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Failed {
    pub path: PathBuf,
    pub node: Option<NodeId>,
    pub category: Category,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub moved: Vec<Moved>,
    pub failed: Vec<Failed>,
}

impl Outcome {
    pub fn moved_bytes(&self) -> u64 {
        self.moved.iter().map(|item| item.size).sum()
    }
}

/// Moves each item to the Trash on its volume. Each one is checked first: it must pass the
/// safety rules, be the same file or folder that was added, and not belong to a running app.
/// Items that fail are left where they are and listed in [`Outcome::failed`].
pub fn move_to_trash(
    items: &[BasketItem],
    places: &Places,
    scan_root: &Path,
    running: &RunningApps,
    inventory: &Inventory,
) -> Outcome {
    let mut outcome = Outcome::default();
    for item in items {
        let fail = |reason: String| Failed {
            path: item.path.clone(),
            node: item.node,
            category: item.category,
            reason,
        };
        if let Some(proof) = &item.proof
            && let Err(reason) = proof.check(&item.path, inventory, places)
        {
            outcome.failed.push(fail(reason.into()));
            continue;
        }
        let metadata = match safety::check(&item.path, places, scan_root) {
            Ok(metadata) => metadata,
            Err(Refusal::ManagedByApp)
                if item.proof.as_ref().is_some_and(LeftoverProof::is_container) =>
            {
                match safety::read_leftover_container(&item.path, scan_root) {
                    Ok(metadata) => metadata,
                    Err(refusal) => {
                        outcome.failed.push(fail(refusal.reason().into()));
                        continue;
                    }
                }
            }
            Err(refusal) => {
                outcome.failed.push(fail(refusal.reason().into()));
                continue;
            }
        };
        if Identity::of(&metadata) != item.identity {
            outcome.failed.push(fail(Refusal::Changed.reason().into()));
            continue;
        }
        if let Some(app) = in_use(item, places, running) {
            outcome
                .failed
                .push(fail(format!("{app} is running; quit it and try again")));
            continue;
        }
        match trash_item(&item.path) {
            Ok(trashed) => outcome.moved.push(Moved {
                path: item.path.clone(),
                trashed,
                node: item.node,
                category: item.category,
                size: item.size,
            }),
            Err(reason) => outcome.failed.push(fail(reason)),
        }
    }
    outcome
}

/// The app or tool using a cache that is about to be moved, if it is running now.
fn in_use(item: &BasketItem, places: &Places, running: &RunningApps) -> Option<String> {
    let relative = item.path.strip_prefix(&places.home).ok()?;
    let names: Vec<&OsStr> = relative.iter().collect();
    let is = |index: usize, name: &str| names.get(index).is_some_and(|n| *n == name);
    if is(0, "Library") && is(1, "Developer") && is(2, "Xcode") && is(3, "DerivedData") {
        return (running.is_app_running("Xcode") || running.is_app_running("Xcode-beta"))
            .then(|| "Xcode".into());
    }
    if is(0, "Library") && is(1, "Caches") {
        let cache = names.get(2)?.to_string_lossy();
        let commands: &[&str] = match cache.to_lowercase().as_str() {
            "homebrew" => &["brew"],
            "pip" => &["pip", "pip3"],
            "yarn" => &["yarn"],
            _ => &[],
        };
        if !commands.is_empty() {
            return running
                .is_command_running(commands)
                .then(|| cache.into_owned());
        }
        return running.owns_cache(&cache).then(|| "Its app".into());
    }
    let tool: Option<(&str, &[&str])> = if is(0, ".npm") {
        Some(("npm", &["npm", "npx"]))
    } else if is(0, ".cargo") {
        Some(("Cargo", &["cargo", "rustc"]))
    } else if is(0, "Library") && is(1, "pnpm") {
        Some(("pnpm", &["pnpm"]))
    } else {
        None
    };
    let (label, commands) = tool?;
    running.is_command_running(commands).then(|| label.into())
}

fn trash_item(path: &Path) -> Result<Option<PathBuf>, String> {
    autoreleasepool(|_| {
        let path = path.to_str().ok_or("The name can't be read")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let mut resulting: Option<Retained<NSURL>> = None;
        let manager = NSFileManager::defaultManager();
        match manager.trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting)) {
            Ok(()) => Ok(resulting
                .and_then(|url| url.path())
                .map(|path| PathBuf::from(path.to_string()))),
            Err(error) => {
                let domain = error.domain().to_string();
                let code = error.code();
                // Only the domain and code are reported: the description names the file.
                tracing::error!(domain, code, "moving an item to the Trash failed");
                Err(trash_error_reason(&domain, code))
            }
        }
    })
}

fn trash_error_reason(domain: &str, code: isize) -> String {
    match (domain, code) {
        ("NSCocoaErrorDomain", 513) => "Permission denied".into(),
        ("NSCocoaErrorDomain", 4) => "No longer exists".into(),
        ("NSCocoaErrorDomain", 640) => "The volume is full".into(),
        ("NSCocoaErrorDomain", 3328) => "This volume has no Trash".into(),
        _ => format!("Couldn't move to the Trash ({domain} {code})"),
    }
}

/// Deletes items this app moved to the Trash, to free their space now. Refuses anything not
/// directly inside a Trash folder. Returns the paths that couldn't be deleted, with reasons.
pub fn delete_permanently(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut failures = Vec::new();
    for path in paths {
        if !is_in_trash(path) {
            failures.push((path.clone(), "Not in the Trash".into()));
            continue;
        }
        let result = match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "deleting an item from the Trash failed");
                failures.push((path.clone(), error.kind().to_string()));
            }
        }
    }
    failures
}

/// `~/.Trash/<item>` or `/Volumes/<volume>/.Trashes/<uid>/<item>`.
fn is_in_trash(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Some(home) = std::env::home_dir()
        && parent == home.join(".Trash")
    {
        return true;
    }
    parent
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == ".Trashes")
        && parent.starts_with("/Volumes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_only_from_the_trash() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("keep.txt");
        std::fs::write(&file, "x").unwrap();

        let failures = delete_permanently(std::slice::from_ref(&file));

        assert_eq!(failures.len(), 1);
        assert!(file.exists());
    }
}
