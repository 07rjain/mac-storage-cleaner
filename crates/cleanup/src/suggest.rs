//! Suggestions: an allow-list of places that are known to be safe or worth reviewing. Anything
//! these rules don't recognize is never suggested.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use scanner::{NodeFlags, NodeId, NodeKind, Tree};

use crate::{Category, Places, RunningApps, Safety, safety};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const INSTALLER_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);
const BUILD_FOLDER_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const LARGE_FILE: u64 = 1_000_000_000;
const MIN_BUILD_FOLDER: u64 = 10_000_000;
const MIN_APP_CACHE: u64 = 1_000_000;
const MAX_ITEMS: usize = 50;
const MAX_BUILD_FOLDERS_CHECKED: usize = 300;

const INSTALLER_EXTENSIONS: &[&str] = &["dmg", "pkg", "mpkg", "xip", "iso"];
const DEVICE_SUPPORT: &[&str] = &[
    "iOS DeviceSupport",
    "watchOS DeviceSupport",
    "tvOS DeviceSupport",
    "visionOS DeviceSupport",
    "xrOS DeviceSupport",
    "macOS DeviceSupport",
];
/// Cache folder, label, and the commands that use it.
const PACKAGE_CACHES: &[(&str, &str, &[&str])] = &[
    (".npm/_cacache", "npm download cache", &["npm", "npx"]),
    ("Library/Caches/Yarn", "Yarn download cache", &["yarn"]),
    ("Library/pnpm/store", "pnpm package store", &["pnpm"]),
    ("Library/Caches/pip", "pip download cache", &["pip", "pip3"]),
    (
        ".cargo/registry/cache",
        "Cargo downloaded crates",
        &["cargo", "rustc"],
    ),
    (
        ".cargo/registry/src",
        "Cargo unpacked crate sources",
        &["cargo", "rustc"],
    ),
    ("Library/Caches/Homebrew", "Homebrew downloads", &["brew"]),
];
/// Folders in `~/Library/Caches` already covered by package caches.
const PACKAGE_CACHE_FOLDERS: &[&str] = &["Yarn", "pip", "Homebrew", "pnpm"];
/// Not searched for build folders or large files.
const SKIPPED_HOME_FOLDERS: &[&str] = &["Library", ".Trash", ".cargo", ".rustup", ".npm", ".git"];

#[derive(Debug, Clone)]
pub struct Candidate {
    pub node: NodeId,
    /// As the user knows it (see [`Places::user_path`]).
    pub path: PathBuf,
    pub size: u64,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub category: Category,
    pub items: Vec<Candidate>,
    /// What was left out and why, for example caches of running apps.
    pub skipped: Vec<String>,
}

impl Suggestion {
    pub fn size(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    /// Safe categories go in the basket as a whole when the card is clicked; the others open
    /// for review first.
    pub fn preselected(&self) -> bool {
        self.category.safety() == Safety::SafeToDelete
    }
}

/// Suggestions for a finished scan, largest first. An item is in at most one suggestion.
pub fn suggest(
    tree: &Tree,
    places: &Places,
    running: &RunningApps,
    now: SystemTime,
) -> Vec<Suggestion> {
    let mut rules = Rules {
        tree,
        places,
        running,
        now,
        taken: HashSet::new(),
        git: find_git(),
    };
    let mut suggestions = vec![
        rules.old_installers(),
        rules.derived_data(),
        rules.device_support(),
        rules.archives(),
        rules.package_caches(),
        rules.build_folders(),
        rules.app_caches(),
        rules.logs(),
        rules.large_files(),
    ];
    suggestions.retain(|suggestion| !suggestion.items.is_empty() || !suggestion.skipped.is_empty());
    for suggestion in &mut suggestions {
        suggestion
            .items
            .sort_by_key(|item| std::cmp::Reverse(item.size));
    }
    suggestions.sort_by_key(|suggestion| std::cmp::Reverse(suggestion.size()));
    suggestions
}

struct Rules<'a> {
    tree: &'a Tree,
    places: &'a Places,
    running: &'a RunningApps,
    now: SystemTime,
    taken: HashSet<NodeId>,
    git: Option<PathBuf>,
}

impl Rules<'_> {
    fn node(&self, relative: &str) -> Option<NodeId> {
        let path = Places::tree_path(self.tree.root_path(), &self.places.in_home(relative));
        self.tree
            .find(&path)
            .filter(|&id| !self.tree.flags(id).contains(NodeFlags::REMOVED))
    }

    /// Where build folders and large files are looked for: the home folder, or the scanned
    /// folder if it is inside the home folder or on an external drive.
    fn search_root(&self) -> Option<NodeId> {
        self.node("").or_else(|| {
            let root = Places::user_path(self.tree.root_path());
            (root.starts_with(&self.places.home) || root.starts_with("/Volumes"))
                .then(|| self.tree.root())
        })
    }

    fn user_path(&self, id: NodeId) -> PathBuf {
        Places::user_path(&self.tree.path(id))
    }

    /// Not removed, not in the cloud, has a size, passes the safety rules, and isn't inside
    /// an item another rule already took.
    fn usable(&self, id: NodeId) -> bool {
        let flags = self.tree.flags(id);
        if flags.contains(NodeFlags::REMOVED)
            || flags.contains(NodeFlags::DATALESS)
            || flags.contains(NodeFlags::MOUNT_POINT)
            || self.tree.allocated(id) == 0
        {
            return false;
        }
        let mut current = Some(id);
        while let Some(node) = current {
            if self.taken.contains(&node) {
                return false;
            }
            current = self.tree.parent(node);
        }
        safety::check_path(&self.user_path(id), self.places).is_ok()
    }

    fn candidate(&self, id: NodeId, note: Option<String>) -> Candidate {
        Candidate {
            node: id,
            path: self.user_path(id),
            size: self.tree.allocated(id),
            note,
        }
    }

    fn finish(
        &mut self,
        category: Category,
        mut items: Vec<Candidate>,
        skipped: Vec<String>,
    ) -> Suggestion {
        items.sort_by_key(|item| std::cmp::Reverse(item.size));
        if category == Category::LargeFiles || category == Category::BuildFolders {
            items.truncate(MAX_ITEMS);
        }
        self.taken.extend(items.iter().map(|item| item.node));
        Suggestion {
            category,
            items,
            skipped,
        }
    }

    fn age(&self, path: &Path) -> Option<Duration> {
        let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
        Some(self.now.duration_since(modified).unwrap_or_default())
    }

    fn old_installers(&mut self) -> Suggestion {
        let mut items = Vec::new();
        for folder in ["Downloads", "Desktop"] {
            let Some(root) = self.node(folder) else {
                continue;
            };
            let mut stack = vec![(root, 0)];
            while let Some((id, depth)) = stack.pop() {
                for child in self.tree.children(id) {
                    match self.tree.kind(child) {
                        NodeKind::Directory if depth < 3 && !is_package(self.tree.name(child)) => {
                            stack.push((child, depth + 1));
                        }
                        NodeKind::File
                            if has_extension(self.tree.name(child), INSTALLER_EXTENSIONS)
                                && self.usable(child) =>
                        {
                            let path = self.user_path(child);
                            if let Some(age) = self.age(&path)
                                && age >= INSTALLER_AGE
                            {
                                items.push(self.candidate(child, Some(age_note(age))));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        self.finish(Category::OldInstallers, items, Vec::new())
    }

    fn derived_data(&mut self) -> Suggestion {
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        if let Some(root) = self.node("Library/Developer/Xcode/DerivedData") {
            if self.running.is_app_running("Xcode") || self.running.is_app_running("Xcode-beta") {
                skipped.push("Skipped while Xcode is running".into());
            } else {
                items = self
                    .tree
                    .children(root)
                    .filter(|&child| self.usable(child))
                    .map(|child| self.candidate(child, None))
                    .collect();
            }
        }
        self.finish(Category::XcodeDerivedData, items, skipped)
    }

    fn device_support(&mut self) -> Suggestion {
        let mut items = Vec::new();
        for platform in DEVICE_SUPPORT {
            let Some(root) = self.node(&format!("Library/Developer/Xcode/{platform}")) else {
                continue;
            };
            let mut versions: Vec<(Vec<u32>, NodeId)> = self
                .tree
                .children(root)
                .filter(|&child| self.tree.kind(child) == NodeKind::Directory)
                .filter_map(|child| Some((os_version(self.tree.name(child))?, child)))
                .collect();
            versions.sort();
            let os = platform.trim_end_matches(" DeviceSupport");
            if let Some(((newest, _), older)) = versions.split_last() {
                let newest = format_version(newest);
                for (version, child) in older {
                    if self.usable(*child) {
                        let note =
                            format!("{os} {}, newest kept is {newest}", format_version(version));
                        items.push(self.candidate(*child, Some(note)));
                    }
                }
            }
        }
        self.finish(Category::XcodeDeviceSupport, items, Vec::new())
    }

    fn archives(&mut self) -> Suggestion {
        let mut items = Vec::new();
        if let Some(root) = self.node("Library/Developer/Xcode/Archives") {
            for day in self.tree.children(root) {
                for archive in self.tree.children(day) {
                    if has_extension(self.tree.name(archive), &["xcarchive"])
                        && self.usable(archive)
                    {
                        let note = self.tree.name(day).to_string_lossy().into_owned();
                        items.push(self.candidate(archive, Some(note)));
                    }
                }
            }
        }
        self.finish(Category::XcodeArchives, items, Vec::new())
    }

    fn package_caches(&mut self) -> Suggestion {
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        for (relative, label, commands) in PACKAGE_CACHES {
            let Some(id) = self.node(relative) else {
                continue;
            };
            if !self.usable(id) {
                continue;
            }
            if self.running.is_command_running(commands) {
                let note = format!("Skipped the {label} while {} is running", commands[0]);
                if !skipped.contains(&note) {
                    skipped.push(note);
                }
                continue;
            }
            items.push(self.candidate(id, Some((*label).to_string())));
        }
        self.finish(Category::PackageCaches, items, skipped)
    }

    fn build_folders(&mut self) -> Suggestion {
        let Some(home) = self.search_root() else {
            return self.finish(Category::BuildFolders, Vec::new(), Vec::new());
        };
        let mut found = Vec::new();
        let mut stack: Vec<NodeId> = self
            .tree
            .children(home)
            .filter(|&child| !is_one_of(self.tree.name(child), SKIPPED_HOME_FOLDERS))
            .collect();
        while let Some(id) = stack.pop() {
            if self.tree.kind(id) != NodeKind::Directory {
                continue;
            }
            if let Some(kind) = self.build_folder_kind(id) {
                if self.tree.allocated(id) >= MIN_BUILD_FOLDER {
                    found.push((id, kind));
                }
                continue;
            }
            let name = self.tree.name(id);
            if name == ".git" || is_package(name) {
                continue;
            }
            stack.extend(self.tree.children(id));
        }
        found.sort_by_key(|&(id, _)| std::cmp::Reverse(self.tree.allocated(id)));
        found.truncate(MAX_BUILD_FOLDERS_CHECKED);

        let mut items = Vec::new();
        let mut in_git = 0;
        for (id, kind) in found {
            if !self.usable(id) {
                continue;
            }
            let path = self.user_path(id);
            let Some(age) =
                newest_change(&path).map(|time| self.now.duration_since(time).unwrap_or_default())
            else {
                continue;
            };
            if age < BUILD_FOLDER_AGE {
                continue;
            }
            if let Some(repository) = self.repository_of(id) {
                match &self.git {
                    Some(git) if !is_tracked(git, &repository, &path) => {}
                    _ => {
                        in_git += 1;
                        continue;
                    }
                }
            }
            let project = path
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            items.push(self.candidate(id, Some(format!("{kind} in {project}, {}", age_note(age)))));
        }
        let skipped = if in_git > 0 {
            vec![format!("Skipped {in_git} tracked by Git or not checkable")]
        } else {
            Vec::new()
        };
        self.finish(Category::BuildFolders, items, skipped)
    }

    fn build_folder_kind(&self, id: NodeId) -> Option<&'static str> {
        let name = self.tree.name(id);
        let parent = self.tree.parent(id)?;
        let sibling = |file: &str| {
            self.tree
                .children(parent)
                .any(|child| self.tree.name(child) == file)
        };
        let child = |file: &str| {
            self.tree
                .children(id)
                .any(|inner| self.tree.name(inner) == file)
        };
        if name == "node_modules" && sibling("package.json") {
            Some("Node modules")
        } else if name == "target"
            && sibling("Cargo.toml")
            && (child("CACHEDIR.TAG") || child(".rustc_info.json"))
        {
            Some("Rust build")
        } else if name == ".build" && sibling("Package.swift") {
            Some("Swift build")
        } else if name == "dist" && sibling("package.json") {
            Some("Build output")
        } else {
            None
        }
    }

    /// The nearest folder above `id` with a `.git` inside, as a user path.
    fn repository_of(&self, id: NodeId) -> Option<PathBuf> {
        let mut current = self.tree.parent(id);
        while let Some(folder) = current {
            if self
                .tree
                .children(folder)
                .any(|child| self.tree.name(child) == ".git")
            {
                return Some(self.user_path(folder));
            }
            current = self.tree.parent(folder);
        }
        None
    }

    fn app_caches(&mut self) -> Suggestion {
        let mut items = Vec::new();
        let mut running = 0;
        if let Some(root) = self.node("Library/Caches") {
            for child in self.tree.children(root) {
                let name = self.tree.name(child).to_string_lossy();
                if name.starts_with("com.apple.")
                    || is_one_of(self.tree.name(child), PACKAGE_CACHE_FOLDERS)
                    || self.tree.allocated(child) < MIN_APP_CACHE
                    || self.tree.flags(child).contains(NodeFlags::UNREADABLE)
                    || !self.usable(child)
                {
                    continue;
                }
                if self.running.owns_cache(&name) {
                    running += 1;
                    continue;
                }
                items.push(self.candidate(child, None));
            }
        }
        let skipped = match running {
            0 => Vec::new(),
            1 => vec!["Skipped 1 cache of a running app".into()],
            count => vec![format!("Skipped {count} caches of running apps")],
        };
        self.finish(Category::AppCaches, items, skipped)
    }

    fn logs(&mut self) -> Suggestion {
        let mut items = Vec::new();
        if let Some(root) = self.node("Library/Logs") {
            items = self
                .tree
                .children(root)
                .filter(|&child| self.usable(child))
                .map(|child| self.candidate(child, None))
                .collect();
        }
        self.finish(Category::Logs, items, Vec::new())
    }

    fn large_files(&mut self) -> Suggestion {
        let mut items = Vec::new();
        if let Some(home) = self.search_root() {
            let mut stack: Vec<NodeId> = self
                .tree
                .children(home)
                .filter(|&child| !is_one_of(self.tree.name(child), SKIPPED_HOME_FOLDERS))
                .collect();
            while let Some(id) = stack.pop() {
                if self.tree.allocated(id) < LARGE_FILE {
                    continue;
                }
                match self.tree.kind(id) {
                    NodeKind::Directory if !is_package(self.tree.name(id)) => {
                        stack.extend(self.tree.children(id));
                    }
                    NodeKind::File if self.usable(id) => {
                        let note = self.age(&self.user_path(id)).map(age_note);
                        items.push(self.candidate(id, note));
                    }
                    _ => {}
                }
            }
        }
        self.finish(Category::LargeFiles, items, Vec::new())
    }
}

fn has_extension(name: &OsStr, extensions: &[&str]) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn is_package(name: &OsStr) -> bool {
    has_extension(
        name,
        &[
            "app",
            "photoslibrary",
            "photolibrary",
            "musiclibrary",
            "framework",
            "bundle",
            "xcarchive",
        ],
    )
}

fn is_one_of(name: &OsStr, names: &[&str]) -> bool {
    name.to_str()
        .is_some_and(|name| names.iter().any(|known| name.eq_ignore_ascii_case(known)))
}

/// The first `major.minor` token, for example `17.2` from `iPhone15,2 17.2 (21C62)`.
fn os_version(name: &OsStr) -> Option<Vec<u32>> {
    let name = name.to_str()?;
    name.split_whitespace().find_map(|token| {
        if !token.contains('.') {
            return None;
        }
        token
            .split('.')
            .map(|part| part.parse::<u32>().ok())
            .collect::<Option<Vec<u32>>>()
    })
}

fn format_version(version: &[u32]) -> String {
    version
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn age_note(age: Duration) -> String {
    let days = age.as_secs() / DAY.as_secs();
    match days {
        0 => "Changed today".into(),
        1 => "Changed yesterday".into(),
        2..=59 => format!("Changed {days} days ago"),
        60..=729 => format!("Changed {} months ago", days / 30),
        _ => format!("Changed {} years ago", days / 365),
    }
}

/// The latest modification time of a folder and the items directly inside it.
fn newest_change(path: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten().take(500) {
            if let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) {
                newest = newest.max(modified);
            }
        }
    }
    Some(newest)
}

/// A real `git`, not the `/usr/bin/git` shim that offers to install developer tools.
fn find_git() -> Option<PathBuf> {
    let developer = Command::new("/usr/bin/xcode-select")
        .arg("-p")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()));
    developer
        .map(|dir| dir.join("usr/bin/git"))
        .into_iter()
        .chain(["/opt/homebrew/bin/git", "/usr/local/bin/git"].map(PathBuf::from))
        .find(|path| path.is_file())
}

fn is_tracked(git: &Path, repository: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(repository) else {
        return true;
    };
    match Command::new(git)
        .arg("-C")
        .arg(repository)
        .args(["ls-files", "-z", "--"])
        .arg(relative)
        .output()
    {
        Ok(output) if output.status.success() => !output.stdout.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_device_support_versions() {
        assert_eq!(
            os_version(OsStr::new("iPhone15,2 17.2 (21C62)")),
            Some(vec![17, 2])
        );
        assert_eq!(
            os_version(OsStr::new("16.4.1 (20E252)")),
            Some(vec![16, 4, 1])
        );
        assert_eq!(os_version(OsStr::new("Logs")), None);
        assert!(vec![17, 2] > vec![16, 4, 1]);
    }

    #[test]
    fn describes_ages() {
        assert_eq!(
            age_note(Duration::from_secs(3 * 86_400)),
            "Changed 3 days ago"
        );
        assert_eq!(
            age_note(Duration::from_secs(120 * 86_400)),
            "Changed 4 months ago"
        );
        assert_eq!(
            age_note(Duration::from_secs(800 * 86_400)),
            "Changed 2 years ago"
        );
    }
}
