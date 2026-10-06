//! What may never go in the basket, whoever asks.

use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::os::macos::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::Places;

const SF_DATALESS: u32 = 0x4000_0000;

/// Top-level folders macOS owns.
const SYSTEM_ROOTS: &[&str] = &[
    "System", "Library", "private", "usr", "bin", "sbin", "etc", "var", "tmp", "cores", "opt",
    "dev", "Users",
];

/// Folders in the home folder that macOS creates and expects to exist.
const STANDARD_FOLDERS: &[&str] = &[
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Movies",
    "Music",
    "Pictures",
    "Public",
    "Sites",
];

/// Packages whose insides belong to the package: removing one file breaks the whole.
const PACKAGES: &[&str] = &[
    "app",
    "appex",
    "bundle",
    "framework",
    "kext",
    "plugin",
    "xpc",
    "photoslibrary",
    "photolibrary",
    "migratedphotolibrary",
    "musiclibrary",
    "tvlibrary",
    "fcpbundle",
    "logicx",
    "band",
    "xcarchive",
];

/// Libraries managed by their app. Their size is shown, but they are cleaned from the app.
const MANAGED_LIBRARIES: &[&str] = &["photoslibrary", "photolibrary", "migratedphotolibrary"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    SystemLocation,
    Applications,
    OutsideHome,
    StandardFolder,
    InsidePackage,
    ManagedByApp,
    CloudStorage,
    InTrash,
    Dataless,
    SymlinkOutside,
    Missing,
    Changed,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Self::SystemLocation => "macOS needs this location",
            Self::Applications => "Apps aren't removed in this version",
            Self::OutsideHome => {
                "Only items in your home folder or on external drives can be cleaned"
            }
            Self::StandardFolder => {
                "This folder is part of your Mac's standard layout; clean what's inside it"
            }
            Self::InsidePackage => "Part of an app or library package",
            Self::ManagedByApp => "Managed by its app; free space from the app instead",
            Self::CloudStorage => {
                "Synced with iCloud or a cloud service; deleting here deletes it everywhere"
            }
            Self::InTrash => "Already in the Trash",
            Self::Dataless => "Stored in the cloud, not on this Mac",
            Self::SymlinkOutside => "A link to somewhere outside the scanned folder",
            Self::Missing => "No longer exists",
            Self::Changed => "Changed since it was added; add it again to review it",
        }
    }
}

/// Rules that depend only on where `path` is. `path` is a user path (see
/// [`Places::user_path`]).
pub fn check_path(path: &Path, places: &Places) -> Result<(), Refusal> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Refusal::OutsideHome);
    }
    if let Ok(rest) = path.strip_prefix("/Volumes") {
        return check_external(rest);
    }
    if let Ok(rest) = path.strip_prefix(&places.home) {
        return check_home(rest);
    }
    let first = path.components().nth(1).map(Component::as_os_str);
    match first {
        Some(name) if name == "Applications" => Err(Refusal::Applications),
        Some(name) if SYSTEM_ROOTS.iter().any(|root| eq(name, root)) => {
            Err(Refusal::SystemLocation)
        }
        _ => Err(Refusal::OutsideHome),
    }
}

/// Every rule: [`check_path`], then what the item on disk is now. Returns its metadata.
/// `scan_root` is the user path of the folder that was scanned.
pub fn check(path: &Path, places: &Places, scan_root: &Path) -> Result<Metadata, Refusal> {
    check_path(path, places)?;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Refusal::Missing),
        Err(_) => return Err(Refusal::Missing),
    };
    if metadata.st_flags() & SF_DATALESS != 0 {
        return Err(Refusal::Dataless);
    }
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|_| Refusal::Missing)?;
        let resolved = normalize(&path.parent().unwrap_or(Path::new("/")).join(target));
        if !resolved.starts_with(scan_root) {
            return Err(Refusal::SymlinkOutside);
        }
    }
    Ok(metadata)
}

/// Metadata for a container folder the leftover proof already accepted. [`check_path`] still
/// refuses that folder; this only reads what is on disk.
pub(crate) fn read_leftover_container(path: &Path, scan_root: &Path) -> Result<Metadata, Refusal> {
    if !path.starts_with(scan_root) {
        return Err(Refusal::OutsideHome);
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Refusal::Missing),
        Err(_) => return Err(Refusal::Missing),
    };
    if metadata.st_flags() & SF_DATALESS != 0 {
        return Err(Refusal::Dataless);
    }
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Refusal::Changed);
    }
    Ok(metadata)
}

fn check_home(rest: &Path) -> Result<(), Refusal> {
    let names: Vec<&OsStr> = rest.components().map(Component::as_os_str).collect();
    let Some(&first) = names.first() else {
        return Err(Refusal::StandardFolder);
    };
    if eq(first, ".Trash") {
        return Err(Refusal::InTrash);
    }
    if eq(first, "Applications") && names.len() > 1 {
        return Err(Refusal::Applications);
    }
    if names.len() == 1 && STANDARD_FOLDERS.iter().any(|folder| eq(first, folder)) {
        return Err(Refusal::StandardFolder);
    }
    if eq(first, "Library") {
        check_library(&names[1..])?;
    }
    check_packages(&names)
}

fn check_library(names: &[&OsStr]) -> Result<(), Refusal> {
    let Some(&folder) = names.first() else {
        return Err(Refusal::StandardFolder);
    };
    if names.len() == 1 {
        return Err(Refusal::StandardFolder);
    }
    if ["Mobile Documents", "CloudStorage"]
        .iter()
        .any(|n| eq(folder, n))
    {
        return Err(Refusal::CloudStorage);
    }
    if ["Keychains", "Preferences", "Accounts", "Cookies", "Sharing"]
        .iter()
        .any(|n| eq(folder, n))
    {
        return Err(Refusal::SystemLocation);
    }
    if ["Mail", "Messages", "Photos"].iter().any(|n| eq(folder, n)) {
        return Err(Refusal::ManagedByApp);
    }
    if eq(folder, "Application Support") && names.get(1).is_some_and(|n| eq(n, "MobileSync")) {
        return Err(Refusal::ManagedByApp);
    }
    if ["Containers", "Group Containers"]
        .iter()
        .any(|n| eq(folder, n))
    {
        if names.len() == 2 {
            return Err(Refusal::ManagedByApp);
        }
        if names[1].to_string_lossy().contains("com.apple.") {
            return Err(Refusal::ManagedByApp);
        }
    }
    Ok(())
}

fn check_external(rest: &Path) -> Result<(), Refusal> {
    let names: Vec<&OsStr> = rest.components().map(Component::as_os_str).collect();
    let Some(&volume) = names.first() else {
        return Err(Refusal::SystemLocation);
    };
    if names.len() == 1 {
        return Err(Refusal::StandardFolder);
    }
    // `/Volumes/Macintosh HD` is a link back to the startup disk.
    if Path::new("/Volumes").join(volume).is_symlink() {
        return Err(Refusal::SystemLocation);
    }
    let second = names[1];
    if [
        ".Trashes",
        ".Spotlight-V100",
        ".fseventsd",
        ".DocumentRevisions-V100",
    ]
    .iter()
    .any(|n| eq(second, n))
    {
        return Err(if eq(second, ".Trashes") {
            Refusal::InTrash
        } else {
            Refusal::SystemLocation
        });
    }
    if ["System", "Library", "private", "usr", "Applications"]
        .iter()
        .any(|n| eq(second, n))
    {
        return Err(Refusal::SystemLocation);
    }
    check_packages(&names[1..])
}

fn check_packages(names: &[&OsStr]) -> Result<(), Refusal> {
    let (last, ancestors) = names.split_last().expect("at least one name");
    if ancestors.iter().any(|name| has_extension(name, PACKAGES)) {
        return Err(Refusal::InsidePackage);
    }
    if has_extension(last, MANAGED_LIBRARIES) {
        return Err(Refusal::ManagedByApp);
    }
    Ok(())
}

fn has_extension(name: &OsStr, extensions: &[&str]) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extensions.iter().any(|known| eq(extension, known)))
}

/// File names on APFS are case-insensitive by default.
fn eq(name: &OsStr, expected: &str) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

/// Resolves `.` and `..` without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places() -> Places {
        Places {
            home: PathBuf::from("/Users/test"),
        }
    }

    fn refusal(path: &str) -> Option<Refusal> {
        check_path(Path::new(path), &places()).err()
    }

    #[test]
    fn refuses_system_locations_and_apps() {
        assert_eq!(refusal("/"), Some(Refusal::OutsideHome));
        assert_eq!(
            refusal("/System/Library/Caches"),
            Some(Refusal::SystemLocation)
        );
        assert_eq!(refusal("/Library/Caches/x"), Some(Refusal::SystemLocation));
        assert_eq!(
            refusal("/private/var/folders"),
            Some(Refusal::SystemLocation)
        );
        assert_eq!(
            refusal("/Applications/Xcode.app"),
            Some(Refusal::Applications)
        );
        assert_eq!(
            refusal("/Users/other/Downloads/a.dmg"),
            Some(Refusal::SystemLocation)
        );
        assert_eq!(refusal("/Users/Shared/x"), Some(Refusal::SystemLocation));
        assert_eq!(refusal("relative/path"), Some(Refusal::OutsideHome));
        assert_eq!(
            refusal("/Users/test/Downloads/../../other"),
            Some(Refusal::OutsideHome)
        );
    }

    #[test]
    fn refuses_the_home_folder_and_its_standard_folders() {
        assert_eq!(refusal("/Users/test"), Some(Refusal::StandardFolder));
        assert_eq!(
            refusal("/Users/test/Downloads"),
            Some(Refusal::StandardFolder)
        );
        assert_eq!(
            refusal("/Users/test/library"),
            Some(Refusal::StandardFolder)
        );
        assert_eq!(
            refusal("/Users/test/Library/Caches"),
            Some(Refusal::StandardFolder)
        );
        assert_eq!(
            refusal("/Users/test/.Trash/old.dmg"),
            Some(Refusal::InTrash)
        );
        assert_eq!(
            refusal("/Users/test/Applications/Tool.app"),
            Some(Refusal::Applications)
        );
        assert_eq!(refusal("/Users/test/Downloads/old.dmg"), None);
        assert_eq!(refusal("/Users/test/Library/Caches/com.example.app"), None);
        assert_eq!(refusal("/Users/test/code/app/node_modules"), None);
    }

    #[test]
    fn refuses_package_internals_and_app_managed_data() {
        assert_eq!(
            refusal("/Users/test/Downloads/Tool.app/Contents/MacOS/tool"),
            Some(Refusal::InsidePackage)
        );
        assert_eq!(refusal("/Users/test/Downloads/Tool.app"), None);
        assert_eq!(
            refusal("/Users/test/Pictures/Photos Library.photoslibrary"),
            Some(Refusal::ManagedByApp)
        );
        assert_eq!(
            refusal("/Users/test/Pictures/Photos Library.photoslibrary/originals/a.heic"),
            Some(Refusal::InsidePackage)
        );
        assert_eq!(
            refusal("/Users/test/Library/Application Support/MobileSync/Backup/abc"),
            Some(Refusal::ManagedByApp)
        );
        assert_eq!(
            refusal("/Users/test/Library/Messages/Attachments"),
            Some(Refusal::ManagedByApp)
        );
        assert_eq!(
            refusal("/Users/test/Library/Mobile Documents/com~apple~CloudDocs/a"),
            Some(Refusal::CloudStorage)
        );
        assert_eq!(
            refusal("/Users/test/Library/Containers/com.apple.mail/Data"),
            Some(Refusal::ManagedByApp)
        );
        assert_eq!(
            refusal("/Users/test/Library/Keychains/login.keychain-db"),
            Some(Refusal::SystemLocation)
        );
    }

    #[test]
    fn allows_external_drives_but_not_their_system_folders() {
        assert_eq!(refusal("/Volumes/Backup"), Some(Refusal::StandardFolder));
        assert_eq!(refusal("/Volumes/Backup/old/movie.mov"), None);
        assert_eq!(
            refusal("/Volumes/Backup/.Trashes/501"),
            Some(Refusal::InTrash)
        );
        assert_eq!(
            refusal("/Volumes/Backup/System"),
            Some(Refusal::SystemLocation)
        );
    }

    #[test]
    fn checks_what_is_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let places = Places {
            home: home.path().to_path_buf(),
        };
        let project = home.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("file.txt"), "x").unwrap();
        std::os::unix::fs::symlink("file.txt", project.join("inside")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", project.join("outside")).unwrap();

        assert!(check(&project.join("file.txt"), &places, home.path()).is_ok());
        assert!(check(&project.join("inside"), &places, home.path()).is_ok());
        assert_eq!(
            check(&project.join("outside"), &places, home.path()).err(),
            Some(Refusal::SymlinkOutside)
        );
        assert_eq!(
            check(&project.join("gone"), &places, home.path()).err(),
            Some(Refusal::Missing)
        );
    }
}
