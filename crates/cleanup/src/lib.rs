//! Cleanup for Mac Storage Cleaner: suggestion rules, the review basket, moving to the Trash,
//! and the local operation log. Nothing here deletes without a basket confirmation, and every
//! path goes through [`safety`] first.

mod basket;
mod copies;
mod inventory;
mod log;
mod managed;
mod processes;
mod restore;
pub mod safety;
mod suggest;
mod trash;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use basket::{AddError, Basket, BasketItem};
pub use copies::{CopyEnd, CopyProof, CopyReport, CopySearch};
pub use inventory::{Inventory, LeftoverProof, STALE_REASON};
pub use log::{Action, ItemState, LogEntry, LoggedItem, OperationLog, PUT_BACK_UNAVAILABLE};
pub use managed::{ManagedKind, ManagedPlace, Opener, managed_places};
pub use processes::RunningApps;
pub use restore::{RestoreFailure, put_back, put_back_at, put_back_items};
pub use safety::Refusal;
pub use suggest::{Candidate, Suggestion, suggest};
pub use trash::{Failed, Moved, Outcome, delete_permanently, move_to_trash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    OldInstallers,
    XcodeDerivedData,
    XcodeDeviceSupport,
    XcodeArchives,
    PackageCaches,
    BuildFolders,
    AppCaches,
    Logs,
    LargeFiles,
    /// Support files for a bundle ID that was not found under Applications.
    Leftovers,
    /// An older exact copy whose removal would free private space.
    ExactCopies,
    /// Old screenshots in the folder Screen Capture uses.
    Screenshots,
    /// Added by the user from the chart or the list.
    Chosen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    SafeToDelete,
    ReviewFirst,
}

impl Category {
    pub fn title(self) -> &'static str {
        match self {
            Self::OldInstallers => "Old installers",
            Self::XcodeDerivedData => "Xcode DerivedData",
            Self::XcodeDeviceSupport => "Old device support files",
            Self::XcodeArchives => "Xcode archives",
            Self::PackageCaches => "Package manager caches",
            Self::BuildFolders => "Project build folders",
            Self::AppCaches => "App caches",
            Self::Logs => "Logs",
            Self::LargeFiles => "Large files",
            Self::Leftovers => "Leftover files not tied to an installed app",
            Self::ExactCopies => "Exact copies",
            Self::Screenshots => "Screenshots",
            Self::Chosen => "Chosen by you",
        }
    }

    /// One line on why the items can go, or what to check first.
    pub fn reason(self) -> &'static str {
        match self {
            Self::OldInstallers => {
                "Disk images and installers in Downloads or Desktop untouched for 90 days"
            }
            Self::XcodeDerivedData => {
                "Build products and indexes Xcode recreates on the next build"
            }
            Self::XcodeDeviceSupport => "Symbols for older OS versions; the newest per OS is kept",
            Self::XcodeArchives => {
                "App builds you archived; keep any you may need to re-submit or symbolicate"
            }
            Self::PackageCaches => {
                "Downloads that npm, Yarn, pnpm, pip, Cargo and Homebrew fetch again when needed"
            }
            Self::BuildFolders => {
                "Dependencies and build output untouched for 7 days and not in Git"
            }
            Self::AppCaches => "Data apps rebuild; an app may start slower once",
            Self::Logs => "Old app and system logs",
            Self::LargeFiles => "Files of 1 GB or more; check each one",
            Self::Leftovers => {
                "Bundle IDs not found under Applications on this Mac. A cache is recreated; other folders may contain saved data"
            }
            Self::ExactCopies => {
                "Identical files. Only a copy that would free its own space is listed"
            }
            Self::Screenshots => {
                "Screenshots in the screenshot folder, untouched for 30 days. Check each one"
            }
            Self::Chosen => "Items you added",
        }
    }

    pub fn safety(self) -> Safety {
        match self {
            Self::OldInstallers | Self::XcodeDerivedData | Self::XcodeDeviceSupport => {
                Safety::SafeToDelete
            }
            _ => Safety::ReviewFirst,
        }
    }
}

/// Where the user's folders are, and how scan paths map to the paths the user knows.
#[derive(Debug, Clone)]
pub struct Places {
    pub home: PathBuf,
}

impl Places {
    pub fn current() -> Option<Self> {
        Some(Self {
            home: std::env::home_dir()?,
        })
    }

    /// The path as the user and Finder know it. A startup-disk scan walks the Data volume, so
    /// its paths start with `/System/Volumes/Data`, which firmlinks hide.
    pub fn user_path(path: &Path) -> PathBuf {
        match path.strip_prefix(volumes::DATA_VOLUME_MOUNT_POINT) {
            Ok(rest) => Path::new("/").join(rest),
            Err(_) => path.to_path_buf(),
        }
    }

    /// `path` as it appears in a tree rooted at `tree_root`.
    pub fn tree_path(tree_root: &Path, path: &Path) -> PathBuf {
        if tree_root.starts_with(volumes::DATA_VOLUME_MOUNT_POINT)
            && !path.starts_with(volumes::DATA_VOLUME_MOUNT_POINT)
        {
            Path::new(volumes::DATA_VOLUME_MOUNT_POINT).join(path.strip_prefix("/").unwrap_or(path))
        } else {
            path.to_path_buf()
        }
    }

    pub fn in_home(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_data_volume_paths_both_ways() {
        let data = Path::new("/System/Volumes/Data/Users/a/Downloads");
        assert_eq!(Places::user_path(data), Path::new("/Users/a/Downloads"));
        assert_eq!(
            Places::tree_path(Path::new("/System/Volumes/Data"), Path::new("/Users/a")),
            Path::new("/System/Volumes/Data/Users/a")
        );
        assert_eq!(
            Places::tree_path(Path::new("/Users/a"), Path::new("/Users/a/x")),
            Path::new("/Users/a/x")
        );
    }
}
