//! The review basket: what the user chose to move to the Trash, checked when added and again
//! right before the move.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use scanner::NodeId;

use crate::copies::CopyProof;
use crate::safety::{self, Refusal};
use crate::{Category, Inventory, LeftoverProof, Places};

/// What an item on disk was when it was added, to notice if it was replaced since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
    directory: bool,
    symlink: bool,
}

impl Identity {
    pub(crate) fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            directory: metadata.is_dir(),
            symlink: metadata.file_type().is_symlink(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BasketItem {
    /// As the user knows it (see [`Places::user_path`]).
    pub path: PathBuf,
    pub node: Option<NodeId>,
    pub category: Category,
    /// From the scan: what the item takes up, before checking clones and hard links.
    pub size: u64,
    pub(crate) identity: Identity,
    pub(crate) proof: Option<LeftoverProof>,
    pub(crate) copy: Option<CopyProof>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    Refused(Refusal),
    /// Already in the basket through this enclosing item.
    AlreadyInside(PathBuf),
    /// The leftover proof no longer matches the app list or the folder.
    Stale,
    /// The exact-copy check failed.
    Copy(String),
}

impl AddError {
    pub fn reason(&self) -> String {
        match self {
            Self::Refused(refusal) => refusal.reason().into(),
            Self::AlreadyInside(outer) => format!(
                "Already in the basket with {}",
                outer.file_name().map_or_else(
                    || outer.display().to_string(),
                    |name| name.to_string_lossy().into_owned()
                )
            ),
            Self::Stale => crate::STALE_REASON.into(),
            Self::Copy(reason) => reason.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Basket {
    places: Places,
    /// User path of the scanned folder; links may not point outside it.
    scan_root: PathBuf,
    items: Vec<BasketItem>,
}

/// Why a path is allowed into the basket, checked inside `insert`.
enum Admission<'a> {
    Plain,
    Leftover(&'a LeftoverProof, &'a Inventory),
    Copy(&'a CopyProof),
}

impl Basket {
    pub fn new(places: Places, scan_root: &Path) -> Self {
        Self {
            places,
            scan_root: Places::user_path(scan_root),
            items: Vec::new(),
        }
    }

    pub fn places(&self) -> &Places {
        &self.places
    }

    pub fn scan_root(&self) -> &Path {
        &self.scan_root
    }

    /// Adds `path` after the safety checks. Items already in the basket that are inside it
    /// are replaced by it. Returns how many were replaced.
    pub fn add(
        &mut self,
        path: &Path,
        node: Option<NodeId>,
        category: Category,
        size: u64,
    ) -> Result<usize, AddError> {
        self.insert(path, node, category, size, Admission::Plain)
    }

    /// Adds an exact copy after checking that the kept file is still the same.
    pub fn add_copy(
        &mut self,
        path: &Path,
        node: Option<NodeId>,
        size: u64,
        copy: &CopyProof,
    ) -> Result<usize, AddError> {
        self.insert(
            path,
            node,
            Category::ExactCopies,
            size,
            Admission::Copy(copy),
        )
    }

    /// Adds a leftover after checking `proof` against `inventory`. A container folder is still
    /// refused by [`crate::safety::check`] on its own; this is the only way it can be added.
    pub fn add_leftover(
        &mut self,
        path: &Path,
        node: Option<NodeId>,
        size: u64,
        proof: &LeftoverProof,
        inventory: &Inventory,
    ) -> Result<usize, AddError> {
        self.insert(
            path,
            node,
            Category::Leftovers,
            size,
            Admission::Leftover(proof, inventory),
        )
    }

    fn insert(
        &mut self,
        path: &Path,
        node: Option<NodeId>,
        category: Category,
        size: u64,
        admission: Admission<'_>,
    ) -> Result<usize, AddError> {
        let path = Places::user_path(path);
        if let Admission::Copy(copy) = admission
            && let Err(reason) = copy.check(&path, &self.items)
        {
            return Err(AddError::Copy(reason));
        }
        if let Some(outer) = self.items.iter().find(|item| path.starts_with(&item.path)) {
            return Err(AddError::AlreadyInside(outer.path.clone()));
        }
        let metadata = match admission {
            Admission::Leftover(proof, inventory) => {
                proof
                    .check(&path, inventory, &self.places)
                    .map_err(|_| AddError::Stale)?;
                if proof.is_container() {
                    match safety::check(&path, &self.places, &self.scan_root) {
                        Ok(metadata) => metadata,
                        Err(Refusal::ManagedByApp) => {
                            safety::read_leftover_container(&path, &self.scan_root)
                                .map_err(AddError::Refused)?
                        }
                        Err(refusal) => return Err(AddError::Refused(refusal)),
                    }
                } else {
                    safety::check(&path, &self.places, &self.scan_root)
                        .map_err(AddError::Refused)?
                }
            }
            Admission::Plain | Admission::Copy(_) => {
                safety::check(&path, &self.places, &self.scan_root).map_err(AddError::Refused)?
            }
        };
        let before = self.items.len();
        self.items.retain(|item| !item.path.starts_with(&path));
        let replaced = before - self.items.len();
        let (proof, copy) = match admission {
            Admission::Leftover(proof, _) => (Some(proof.clone()), None),
            Admission::Copy(copy) => (None, Some(copy.clone())),
            Admission::Plain => (None, None),
        };
        self.items.push(BasketItem {
            path,
            node,
            category,
            size,
            identity: Identity::of(&metadata),
            proof,
            copy,
        });
        Ok(replaced)
    }

    pub fn remove(&mut self, path: &Path) {
        let path = Places::user_path(path);
        self.items.retain(|item| item.path != path);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn items(&self) -> &[BasketItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn contains(&self, path: &Path) -> bool {
        let path = Places::user_path(path);
        self.items.iter().any(|item| path.starts_with(&item.path))
    }

    /// The scan's sizes, summed. Basket items never overlap, so nothing is counted twice.
    pub fn estimate(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.items.iter().map(|item| item.path.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basket(home: &Path) -> Basket {
        Basket::new(
            Places {
                home: home.to_path_buf(),
            },
            home,
        )
    }

    #[test]
    fn refused_paths_cannot_be_added() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("Library/Caches/app")).unwrap();
        std::fs::create_dir_all(home.path().join("Downloads")).unwrap();
        std::fs::create_dir_all(home.path().join("Pictures/Photos Library.photoslibrary")).unwrap();
        std::fs::create_dir_all(
            home.path()
                .join("Library/Containers/com.apple.mail/Data/Library/Mail Downloads"),
        )
        .unwrap();
        let mut basket = basket(home.path());

        for (path, refusal) in [
            (home.path().to_path_buf(), Refusal::StandardFolder),
            (home.path().join("Library"), Refusal::StandardFolder),
            (home.path().join("Library/Caches"), Refusal::StandardFolder),
            (home.path().join("Downloads"), Refusal::StandardFolder),
            (PathBuf::from("/System/Library"), Refusal::SystemLocation),
            (
                PathBuf::from("/Applications/Safari.app"),
                Refusal::Applications,
            ),
            (home.path().join("Downloads/missing.dmg"), Refusal::Missing),
            (
                home.path().join("Pictures/Photos Library.photoslibrary"),
                Refusal::ManagedByApp,
            ),
            (
                home.path()
                    .join("Library/Containers/com.apple.mail/Data/Library/Mail Downloads"),
                Refusal::ManagedByApp,
            ),
        ] {
            assert_eq!(
                basket.add(&path, None, Category::Chosen, 1),
                Err(AddError::Refused(refusal)),
                "{}",
                path.display()
            );
        }
        assert!(basket.is_empty());
        assert_eq!(
            basket.add(
                &home.path().join("Library/Caches/app"),
                None,
                Category::AppCaches,
                5
            ),
            Ok(0)
        );
    }

    #[test]
    fn nested_items_are_counted_once() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("code/app");
        std::fs::create_dir_all(project.join("node_modules/a")).unwrap();
        let mut basket = basket(home.path());

        basket
            .add(&project.join("node_modules/a"), None, Category::Chosen, 10)
            .unwrap();
        assert_eq!(
            basket.add(
                &project.join("node_modules"),
                None,
                Category::BuildFolders,
                30
            ),
            Ok(1)
        );
        assert_eq!(
            basket.add(&project.join("node_modules/a"), None, Category::Chosen, 10),
            Err(AddError::AlreadyInside(project.join("node_modules")))
        );
        assert_eq!(basket.len(), 1);
        assert_eq!(basket.estimate(), 30);
        assert!(basket.contains(&project.join("node_modules/a")));
    }
}
