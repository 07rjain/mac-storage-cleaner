use std::ffi::OsStr;
use std::ops::{BitOr, BitOrAssign};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub type NodeId = u32;

const NO_NODE: NodeId = NodeId::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeFlags(u8);

impl NodeFlags {
    /// The directory could not be listed, usually for lack of permission.
    pub const UNREADABLE: Self = Self(1 << 0);
    /// The item's contents are not on this Mac. Directories are not descended into.
    pub const DATALESS: Self = Self(1 << 1);
    /// The directory belongs to another file system and was not descended into.
    pub const MOUNT_POINT: Self = Self(1 << 2);
    /// Another path to the same hard-linked file was already counted; this one counts as 0.
    pub const HARD_LINK_DUPLICATE: Self = Self(1 << 3);
    /// The file uses file-system compression.
    pub const COMPRESSED: Self = Self(1 << 4);
    /// The file system reported an error for this entry.
    pub const ENTRY_ERROR: Self = Self(1 << 5);
    /// An unedited APFS clone whose data is counted under another file with the same clone ID.
    pub const SHARED_WITH_CLONE: Self = Self(1 << 6);
    /// Removed with [`Tree::remove`], for example after moving it to the Trash.
    pub const REMOVED: Self = Self(1 << 7);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for NodeFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOrAssign for NodeFlags {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub other: u64,
    pub unreadable_directories: u64,
    pub dataless_items: u64,
    pub mount_points_skipped: u64,
    pub hard_link_duplicates: u64,
    /// Files flagged [`NodeFlags::SHARED_WITH_CLONE`].
    pub clones: u64,
    /// Bytes not counted again because they are shared with a clone counted elsewhere.
    pub clone_shared_bytes: u64,
    /// Edited clones, counted in full because their shared blocks can't be attributed.
    pub edited_clones: u64,
    /// Allocated bytes of edited clones; an upper bound on how much the scan can overcount.
    pub edited_clone_bytes: u64,
    pub entry_errors: u64,
}

impl ScanStats {
    pub fn entries(&self) -> u64 {
        self.files + self.directories + self.symlinks + self.other
    }
}

/// Sums over the entries of one directory listing.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Totals {
    pub allocated: u64,
    pub logical: u64,
    pub items: u32,
}

#[derive(Debug, Clone)]
struct Node {
    parent: NodeId,
    first_child: NodeId,
    next_sibling: NodeId,
    name_start: u32,
    name_len: u16,
    kind: NodeKind,
    flags: NodeFlags,
    items: u32,
    allocated: u64,
    logical: u64,
}

/// Scan result, readable while the scan runs. Node 0 is the scan root, and every other node is
/// inserted after its parent.
///
/// Directory totals are kept current as each directory is listed, so a partial tree shows
/// lower bounds that only grow. [`Tree::is_settled`] tells whether a total is final.
#[derive(Debug, Clone)]
pub struct Tree {
    root_path: PathBuf,
    nodes: Vec<Node>,
    names: Vec<u8>,
    /// Directory listings still outstanding in each node's subtree, including its own. Freed
    /// once the scan completes.
    pending: Vec<u32>,
    stats: ScanStats,
    complete: bool,
}

impl Tree {
    pub(crate) fn new(root_path: PathBuf) -> Self {
        let mut tree = Self {
            root_path,
            nodes: Vec::new(),
            names: Vec::new(),
            pending: Vec::new(),
            stats: ScanStats::default(),
            complete: false,
        };
        let root_name = tree.root_path.as_os_str().as_bytes().to_vec();
        let root = tree.insert(
            NO_NODE,
            &root_name,
            NodeKind::Directory,
            NodeFlags::default(),
            0,
            0,
        );
        tree.mark_pending(root);
        tree
    }

    pub(crate) fn insert(
        &mut self,
        parent: NodeId,
        name: &[u8],
        kind: NodeKind,
        flags: NodeFlags,
        allocated: u64,
        logical: u64,
    ) -> NodeId {
        let id = NodeId::try_from(self.nodes.len()).expect("more than 4 billion entries");
        let name_start = u32::try_from(self.names.len()).expect("name storage exceeds 4 GiB");
        let name = &name[..name.len().min(usize::from(u16::MAX))];
        self.names.extend_from_slice(name);

        let next_sibling = if parent == NO_NODE {
            NO_NODE
        } else {
            std::mem::replace(&mut self.nodes[parent as usize].first_child, id)
        };
        self.nodes.push(Node {
            parent,
            first_child: NO_NODE,
            next_sibling,
            name_start,
            name_len: name.len() as u16,
            kind,
            flags,
            items: 0,
            allocated,
            logical,
        });
        self.pending.push(0);
        id
    }

    /// Records that `id`, a directory just inserted, will be listed.
    pub(crate) fn mark_pending(&mut self, id: NodeId) {
        self.pending[id as usize] = 1;
    }

    /// Records that `id` was listed: adds the totals of its new children to it and every
    /// ancestor, and replaces its own pending listing with the `new_pending` children queued.
    pub(crate) fn finish_listing(&mut self, id: NodeId, children: Totals, new_pending: u32) {
        let mut current = id;
        while current != NO_NODE {
            let node = &mut self.nodes[current as usize];
            node.allocated += children.allocated;
            node.logical += children.logical;
            node.items += children.items;
            let pending = &mut self.pending[current as usize];
            *pending = *pending + new_pending - 1;
            current = node.parent;
        }
    }

    pub(crate) fn add_flags(&mut self, id: NodeId, flags: NodeFlags) {
        self.nodes[id as usize].flags |= flags;
    }

    pub(crate) fn stats_mut(&mut self) -> &mut ScanStats {
        &mut self.stats
    }

    pub(crate) fn finish(&mut self, complete: bool) {
        self.complete = complete;
        if complete {
            self.pending = Vec::new();
        }
    }

    pub fn root(&self) -> NodeId {
        0
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// `true` once every directory was listed; stays `false` if the scan was cancelled.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// `true` if nothing inside `id` is still waiting to be listed, so its totals are final.
    pub fn is_settled(&self, id: NodeId) -> bool {
        self.pending
            .get(id as usize)
            .is_none_or(|&pending| pending == 0)
    }

    pub fn stats(&self) -> &ScanStats {
        &self.stats
    }

    pub fn name(&self, id: NodeId) -> &OsStr {
        let node = &self.nodes[id as usize];
        let start = node.name_start as usize;
        OsStr::from_bytes(&self.names[start..start + usize::from(node.name_len)])
    }

    pub fn kind(&self, id: NodeId) -> NodeKind {
        self.nodes[id as usize].kind
    }

    pub fn flags(&self, id: NodeId) -> NodeFlags {
        self.nodes[id as usize].flags
    }

    /// On-disk bytes. For directories, the total of everything inside.
    pub fn allocated(&self, id: NodeId) -> u64 {
        self.nodes[id as usize].allocated
    }

    /// Apparent (logical) bytes. For directories, the total of everything inside.
    pub fn logical(&self, id: NodeId) -> u64 {
        self.nodes[id as usize].logical
    }

    /// Number of entries inside a directory, at any depth.
    pub fn items(&self, id: NodeId) -> u32 {
        self.nodes[id as usize].items
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes[id as usize].parent;
        (parent != NO_NODE).then_some(parent)
    }

    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            tree: self,
            next: self.nodes[id as usize].first_child,
        }
    }

    /// Children of `id`, largest first by allocated size.
    pub fn children_by_size(&self, id: NodeId) -> Vec<NodeId> {
        let mut children: Vec<NodeId> = self.children(id).collect();
        children.sort_unstable_by_key(|&child| std::cmp::Reverse(self.allocated(child)));
        children
    }

    /// The node at `path`, which must be the root path or inside it.
    pub fn find(&self, path: &Path) -> Option<NodeId> {
        let rest = path.strip_prefix(&self.root_path).ok()?;
        let mut current = self.root();
        for component in rest.components() {
            let name = component.as_os_str();
            current = self
                .children(current)
                .find(|&child| self.name(child) == name)?;
        }
        Some(current)
    }

    /// Detaches `id` and everything inside it, and takes its sizes off every ancestor. Its
    /// name and path stay readable, and it is flagged [`NodeFlags::REMOVED`].
    ///
    /// Returns `false`, changing nothing, for the root, an item already removed, or one whose
    /// totals are still being counted.
    pub fn remove(&mut self, id: NodeId) -> bool {
        let Some(parent) = self.parent(id) else {
            return false;
        };
        if self.flags(id).contains(NodeFlags::REMOVED) || !self.is_settled(id) {
            return false;
        }
        let next = self.nodes[id as usize].next_sibling;
        if self.nodes[parent as usize].first_child == id {
            self.nodes[parent as usize].first_child = next;
        } else {
            let mut sibling = self.nodes[parent as usize].first_child;
            while sibling != NO_NODE {
                if self.nodes[sibling as usize].next_sibling == id {
                    self.nodes[sibling as usize].next_sibling = next;
                    break;
                }
                sibling = self.nodes[sibling as usize].next_sibling;
            }
        }
        let node = &self.nodes[id as usize];
        let (allocated, logical, items) = (node.allocated, node.logical, node.items + 1);
        let mut current = Some(parent);
        while let Some(ancestor) = current {
            let node = &mut self.nodes[ancestor as usize];
            node.allocated = node.allocated.saturating_sub(allocated);
            node.logical = node.logical.saturating_sub(logical);
            node.items = node.items.saturating_sub(items);
            current = self.parent(ancestor);
        }
        self.nodes[id as usize].flags |= NodeFlags::REMOVED;
        true
    }

    /// Replaces what is inside the folder `id` with `rescan`, a finished scan of the same folder,
    /// and corrects every ancestor's totals. The old contents are flagged
    /// [`NodeFlags::REMOVED`].
    ///
    /// Hard links and clones are matched only within `rescan`, so a file sharing data with one
    /// outside the folder may now be counted twice.
    ///
    /// Returns `false`, changing nothing, unless both trees are complete, `id` is a folder other
    /// than the root that was not removed, and `rescan` is rooted at its path.
    pub fn replace(&mut self, id: NodeId, rescan: &Tree) -> bool {
        let Some(parent) = self.parent(id) else {
            return false;
        };
        if !self.complete
            || !rescan.complete
            || self.kind(id) != NodeKind::Directory
            || self.flags(id).contains(NodeFlags::REMOVED)
            || rescan.root_path != self.path(id)
        {
            return false;
        }

        let mut old = vec![id];
        while let Some(node) = old.pop() {
            for child in self.children(node).collect::<Vec<_>>() {
                self.nodes[child as usize].flags |= NodeFlags::REMOVED;
                old.push(child);
            }
        }

        let base = NodeId::try_from(self.nodes.len()).expect("more than 4 billion entries");
        let map = |node: NodeId| match node {
            NO_NODE => NO_NODE,
            0 => id,
            node => base + node - 1,
        };
        for node in &rescan.nodes[1..] {
            let name = &rescan.names
                [node.name_start as usize..node.name_start as usize + usize::from(node.name_len)];
            let name_start = u32::try_from(self.names.len()).expect("name storage exceeds 4 GiB");
            self.names.extend_from_slice(name);
            self.nodes.push(Node {
                parent: map(node.parent),
                first_child: map(node.first_child),
                next_sibling: map(node.next_sibling),
                name_start,
                ..node.clone()
            });
        }

        let new = &rescan.nodes[0];
        let target = &mut self.nodes[id as usize];
        let (old_allocated, old_logical, old_items) =
            (target.allocated, target.logical, target.items);
        target.first_child = map(new.first_child);
        target.allocated = new.allocated;
        target.logical = new.logical;
        target.items = new.items;
        target.flags = new.flags;
        let mut current = Some(parent);
        while let Some(ancestor) = current {
            let node = &mut self.nodes[ancestor as usize];
            node.allocated = node.allocated.saturating_sub(old_allocated) + new.allocated;
            node.logical = node.logical.saturating_sub(old_logical) + new.logical;
            node.items = node.items.saturating_sub(old_items) + new.items;
            current = self.parent(ancestor);
        }
        true
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut components = Vec::new();
        let mut current = id;
        while let Some(parent) = self.parent(current) {
            components.push(self.name(current));
            current = parent;
        }
        let mut path = self.root_path.clone();
        path.extend(components.into_iter().rev());
        path
    }
}

pub struct Children<'a> {
    tree: &'a Tree,
    next: NodeId,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.next == NO_NODE {
            return None;
        }
        let current = self.next;
        self.next = self.tree.nodes[current as usize].next_sibling;
        Some(current)
    }
}
