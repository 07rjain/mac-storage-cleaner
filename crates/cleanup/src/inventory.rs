//! Which apps are on this Mac, and the proof required before a leftover container can be moved.
//!
//! The walk reads `CFBundleIdentifier` from each `.app` and does not search past Applications.
//! A partial or unreadable list is not used. `safety::check` does not learn this list.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::processes::bundle_identifier;
use crate::{Places, RunningApps, managed, safety};

const APP_LIST_UNREADABLE: &str = "The app list could not be read";
const PROCESS_UNREADABLE: &str = "A running process could not be identified";
/// Shown when a leftover is no longer the folder that was reviewed.
pub const STALE_REASON: &str = "The app list changed. Review this item again.";
const MAX_DEPTH: u32 = 32;

/// Bundle IDs found under Applications, plus bundle IDs of running apps whose paths could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    installed: BTreeSet<String>,
    generation: u64,
    complete: bool,
    reason: Option<String>,
    roots: Vec<PathBuf>,
}

impl Inventory {
    /// Reads `/Applications`, `/System/Applications`, and `home/Applications` when that folder exists.
    pub fn for_this_mac(home: &Path, running: &RunningApps) -> Self {
        let applications = PathBuf::from("/Applications");
        let system = PathBuf::from("/System/Applications");
        let home_applications = home.join("Applications");
        Self::scan(&[&applications, &system], &[&home_applications], running)
    }

    /// `required` roots that cannot be read omit the category. `optional` roots may be missing.
    pub fn scan(required: &[&Path], optional: &[&Path], running: &RunningApps) -> Self {
        let mut walk = Walk::default();
        let mut roots = Vec::new();
        for root in required {
            match walk.root(root) {
                RootRead::Read => roots.push(root.to_path_buf()),
                RootRead::Missing | RootRead::Unreadable => {
                    return Self::omitted(APP_LIST_UNREADABLE, roots);
                }
            }
        }
        for root in optional {
            match walk.root(root) {
                RootRead::Read => roots.push(root.to_path_buf()),
                RootRead::Missing => {}
                RootRead::Unreadable => return Self::omitted(APP_LIST_UNREADABLE, roots),
            }
        }
        if walk.incomplete {
            return Self::omitted(APP_LIST_UNREADABLE, roots);
        }
        if walk.unknown > 0 {
            let reason = if walk.unknown == 1 {
                "1 app could not be identified".to_string()
            } else {
                format!("{} apps could not be identified", walk.unknown)
            };
            return Self::omitted(reason, roots);
        }
        if running.has_unreadable_process() {
            return Self::omitted(PROCESS_UNREADABLE, roots);
        }
        walk.ids.extend(running.bundle_ids().map(str::to_string));
        Self::complete(walk.ids, roots)
    }

    /// A finished inventory containing these bundle IDs and nothing unknown. For tests and for
    /// callers that already walked Applications.
    pub fn known(ids: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let installed = ids
            .into_iter()
            .map(|id| id.as_ref().to_lowercase())
            .filter(|id| !id.is_empty())
            .collect();
        Self::complete(installed, Vec::new())
    }

    fn complete(installed: BTreeSet<String>, roots: Vec<PathBuf>) -> Self {
        let generation = generation_of(&installed, true, 0, false);
        Self {
            installed,
            generation,
            complete: true,
            reason: None,
            roots,
        }
    }

    fn omitted(reason: impl Into<String>, roots: Vec<PathBuf>) -> Self {
        let reason = reason.into();
        let generation = generation_of(&BTreeSet::new(), false, 1, false);
        Self {
            installed: BTreeSet::new(),
            generation,
            complete: false,
            reason: Some(reason),
            roots,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn contains(&self, bundle_id: &str) -> bool {
        self.installed.contains(&bundle_id.to_lowercase())
    }

    /// Roots that were listed, for the suggestion sheet.
    pub fn checked_note(&self) -> Option<String> {
        match self.roots.as_slice() {
            [] => None,
            [one] => Some(format!("Checked {}", one.display())),
            [first, second] => Some(format!(
                "Checked {} and {}",
                first.display(),
                second.display()
            )),
            many => {
                let last = many.len() - 1;
                let front = many[..last]
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(format!("Checked {front}, and {}", many[last].display()))
            }
        }
    }
}

/// Captured when a leftover is suggested. Add and confirm both recompute the inventory and
/// refuse the item when this no longer matches the folder on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftoverProof {
    bundle_id: String,
    path: PathBuf,
    identity: ProofIdentity,
    generation: u64,
    container: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProofIdentity {
    device: u64,
    inode: u64,
    directory: bool,
    symlink: bool,
}

impl ProofIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            directory: metadata.is_dir(),
            symlink: metadata.file_type().is_symlink(),
        }
    }
}

impl LeftoverProof {
    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    pub fn is_container(&self) -> bool {
        self.container
    }

    /// Records the folder that was reviewed. Returns nothing when the inventory is incomplete,
    /// the bundle is installed, or the folder is not the file this proof would name.
    pub fn issue(
        path: &Path,
        bundle_id: &str,
        container: bool,
        inventory: &Inventory,
        places: &Places,
    ) -> Option<Self> {
        if !inventory.is_complete() || inventory.contains(bundle_id) || is_apple(bundle_id) {
            return None;
        }
        if !is_bundle_id(bundle_id) {
            return None;
        }
        let path = Places::user_path(path);
        if container && !is_container_folder(&path, places, bundle_id) {
            return None;
        }
        if managed::removal_includes_managed(&path, places) {
            return None;
        }
        // `/var` and `/private/var` name the same folder. Keep the path the scan used.
        std::fs::canonicalize(&path).ok()?;
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        let identity = ProofIdentity::of(&metadata);
        if identity.symlink || (container && !identity.directory) {
            return None;
        }
        Some(Self {
            bundle_id: bundle_id.to_lowercase(),
            path,
            identity,
            generation: inventory.generation(),
            container,
        })
    }

    /// Whether this proof still describes `path` under a current inventory.
    pub fn check(
        &self,
        path: &Path,
        inventory: &Inventory,
        places: &Places,
    ) -> Result<(), &'static str> {
        if !inventory.is_complete() || inventory.generation() != self.generation {
            return Err(STALE_REASON);
        }
        if inventory.contains(&self.bundle_id) || is_apple(&self.bundle_id) {
            return Err(STALE_REASON);
        }
        let path = Places::user_path(path);
        if !same_folder(&path, &self.path) {
            return Err(STALE_REASON);
        }
        if self.container && !is_container_folder(&path, places, &self.bundle_id) {
            return Err(STALE_REASON);
        }
        if managed::removal_includes_managed(&path, places) {
            return Err(STALE_REASON);
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| STALE_REASON)?;
        if ProofIdentity::of(&metadata) != self.identity {
            return Err(STALE_REASON);
        }
        if self.container {
            match safety::check_path(&path, places) {
                Err(safety::Refusal::ManagedByApp) => Ok(()),
                _ => Err(STALE_REASON),
            }
        } else if safety::check_path(&path, places).is_err() {
            Err(STALE_REASON)
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct Walk {
    ids: BTreeSet<String>,
    unknown: u32,
    incomplete: bool,
}

enum RootRead {
    Read,
    Missing,
    Unreadable,
}

impl Walk {
    fn root(&mut self, root: &Path) -> RootRead {
        if self.incomplete {
            return RootRead::Unreadable;
        }
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => RootRead::Missing,
            Err(_) => RootRead::Unreadable,
            Ok(metadata) if metadata.is_dir() => {
                self.visit_dir(root, root, 0, &mut HashSet::new());
                if self.incomplete {
                    RootRead::Unreadable
                } else {
                    RootRead::Read
                }
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                match std::fs::canonicalize(root) {
                    Ok(target) if target.is_dir() => {
                        self.visit_dir(root, root, 0, &mut HashSet::new());
                        if self.incomplete {
                            RootRead::Unreadable
                        } else {
                            RootRead::Read
                        }
                    }
                    _ => RootRead::Unreadable,
                }
            }
            Ok(_) => RootRead::Unreadable,
        }
    }

    fn visit_dir(&mut self, dir: &Path, root: &Path, depth: u32, seen: &mut HashSet<(u64, u64)>) {
        if self.incomplete {
            return;
        }
        if depth > MAX_DEPTH {
            self.incomplete = true;
            return;
        }
        let Ok(metadata) = std::fs::symlink_metadata(dir) else {
            self.incomplete = true;
            return;
        };
        if !seen.insert((metadata.dev(), metadata.ino())) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            self.incomplete = true;
            return;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                self.incomplete = true;
                continue;
            };
            let path = entry.path();
            if is_app_name(&entry.file_name()) {
                self.record_app(&path);
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                self.incomplete = true;
                continue;
            };
            if metadata.is_dir() {
                self.visit_dir(&path, root, depth + 1, seen);
            } else if metadata.file_type().is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(target) if target.file_name().is_some_and(is_app_name) => {
                        self.record_app(&path);
                    }
                    Ok(target) if target.is_dir() && target.starts_with(root) => {
                        self.visit_dir(&path, root, depth + 1, seen);
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        }
    }

    fn record_app(&mut self, path: &Path) {
        let broken = std::fs::symlink_metadata(path)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
            && std::fs::canonicalize(path).is_err();
        if broken {
            self.unknown += 1;
            return;
        }
        match bundle_identifier(path) {
            Some(id) if is_bundle_id(&id) => {
                self.ids.insert(id.to_lowercase());
            }
            _ => self.unknown += 1,
        }
    }
}

fn generation_of(
    installed: &BTreeSet<String>,
    complete: bool,
    unknown_apps: u32,
    unknown_processes: bool,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    installed.hash(&mut hasher);
    complete.hash(&mut hasher);
    unknown_apps.hash(&mut hasher);
    unknown_processes.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn is_bundle_id(name: &str) -> bool {
    let mut labels = name.split('.');
    let (Some(first), Some(second)) = (labels.next(), labels.next()) else {
        return false;
    };
    !first.is_empty()
        && !second.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '.' || character == '-'
        })
}

pub(crate) fn is_apple(bundle_id: &str) -> bool {
    let bundle_id = bundle_id.to_lowercase();
    bundle_id == "com.apple" || bundle_id.starts_with("com.apple.")
}

fn is_app_name(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
}

/// `~/Library/Containers/<bundle-id>` itself, not a file inside it and not an Apple container.
pub(crate) fn is_container_folder(path: &Path, places: &Places, bundle_id: &str) -> bool {
    if is_apple(bundle_id) || !is_bundle_id(bundle_id) {
        return false;
    }
    let Ok(rest) = path.strip_prefix(&places.home) else {
        return false;
    };
    let names: Vec<&OsStr> = rest.components().map(Component::as_os_str).collect();
    names.len() == 3
        && eq(names[0], "Library")
        && eq(names[1], "Containers")
        && eq(names[2], bundle_id)
}

fn same_folder(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => Places::user_path(&left) == Places::user_path(&right),
        _ => false,
    }
}

fn eq(name: &OsStr, expected: &str) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plist(app: &Path, bundle_id: &str) {
        let contents = app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string></dict></plist>"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn nested_apps_count_and_a_missing_optional_root_does_not() {
        let root = tempfile::tempdir().unwrap();
        let applications = root.path().join("Applications");
        plist(
            &applications.join("Vendor/Editor.app"),
            "com.example.editor",
        );
        let missing = root.path().join("no-applications");

        let inventory = Inventory::scan(&[&applications], &[&missing], &RunningApps::default());

        assert!(inventory.is_complete());
        assert!(inventory.contains("com.example.editor"));
        assert!(inventory.checked_note().unwrap().contains("Applications"));
        assert!(
            !inventory
                .checked_note()
                .unwrap()
                .contains("no-applications")
        );
    }

    #[test]
    fn an_unreadable_app_or_root_omits_the_list() {
        let root = tempfile::tempdir().unwrap();
        let applications = root.path().join("Applications");
        std::fs::create_dir_all(applications.join("Broken.app/Contents")).unwrap();

        let inventory = Inventory::scan(&[&applications], &[], &RunningApps::default());

        assert!(!inventory.is_complete());
        assert_eq!(inventory.reason(), Some("1 app could not be identified"));

        let blocked = root.path().join("locked");
        std::fs::create_dir(&blocked).unwrap();
        let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o0);
        std::fs::set_permissions(&blocked, permissions).unwrap();
        let unreadable = Inventory::scan(&[&blocked], &[], &RunningApps::default());
        let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&blocked, permissions).unwrap();

        assert!(!unreadable.is_complete());
        assert_eq!(unreadable.reason(), Some(APP_LIST_UNREADABLE));
    }

    #[test]
    fn a_broken_app_symlink_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        let applications = root.path().join("Applications");
        std::fs::create_dir(&applications).unwrap();
        std::os::unix::fs::symlink(
            applications.join("Missing.app"),
            applications.join("Gone.app"),
        )
        .unwrap();

        let inventory = Inventory::scan(&[&applications], &[], &RunningApps::default());

        assert_eq!(inventory.reason(), Some("1 app could not be identified"));
    }

    #[test]
    fn an_unreadable_process_omits_a_finished_walk() {
        let root = tempfile::tempdir().unwrap();
        let applications = root.path().join("Applications");
        plist(&applications.join("Editor.app"), "com.example.editor");
        let running = RunningApps::default().with_unreadable_process();

        let inventory = Inventory::scan(&[&applications], &[], &running);

        assert!(!inventory.is_complete());
        assert_eq!(inventory.reason(), Some(PROCESS_UNREADABLE));
        assert!(!inventory.contains("com.example.editor"));
    }
}
