//! File-type bars for the current folder.
//!
//! Direct files are classified from their names. The largest child folders are opened one level
//! so a folder of folders still shows what is inside. Anything past the visit budget stays in
//! one Folders share, which keeps a huge directory from stalling the window.

use std::cmp::Reverse;

use scanner::{NodeId, NodeKind, Tree};

use crate::file_types::FileType;

/// Largest child folders opened one level. The rest stay in the Folders share.
const OPEN_FOLDERS: usize = 16;
/// Nodes read while classifying. The unread tail of a folder is kept as one share.
const MAX_VISITS: usize = 40_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Kind(FileType),
    /// Directories that were not opened, so their files are not split by type.
    Folders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Share {
    pub bucket: Bucket,
    pub size: u64,
}

impl Bucket {
    pub fn title(self) -> &'static str {
        match self {
            Self::Kind(kind) => kind.title(),
            Self::Folders => "Folders",
        }
    }
}

/// `deep` opens the largest child folders. While a scan is still running, pass `false`.
pub fn breakdown(tree: &Tree, folder: NodeId, deep: bool) -> Vec<Share> {
    let mut sizes = [0u64; FileType::ALL.len()];
    let mut folders = 0u64;
    let mut visits = 0usize;
    let mut pending = Vec::new();

    absorb(
        tree,
        folder,
        true,
        &mut sizes,
        &mut folders,
        &mut visits,
        &mut pending,
    );

    if deep {
        pending.sort_unstable_by_key(|&(id, size)| (Reverse(size), id));
        if pending.len() > OPEN_FOLDERS {
            folders += pending[OPEN_FOLDERS..]
                .iter()
                .map(|(_, size)| *size)
                .sum::<u64>();
            pending.truncate(OPEN_FOLDERS);
        }
        for (id, size) in pending {
            if visits >= MAX_VISITS {
                folders += size;
                continue;
            }
            absorb(
                tree,
                id,
                false,
                &mut sizes,
                &mut folders,
                &mut visits,
                &mut Vec::new(),
            );
        }
    } else {
        folders += pending.into_iter().map(|(_, size)| size).sum::<u64>();
    }

    let mut shares: Vec<Share> = FileType::ALL
        .into_iter()
        .filter_map(|kind| {
            let size = sizes[kind_index(kind)];
            (size > 0).then_some(Share {
                bucket: Bucket::Kind(kind),
                size,
            })
        })
        .collect();
    if folders > 0 {
        shares.push(Share {
            bucket: Bucket::Folders,
            size: folders,
        });
    }
    shares.sort_unstable_by_key(|share| (Reverse(share.size), bucket_order(share.bucket)));
    shares
}

fn absorb(
    tree: &Tree,
    id: NodeId,
    keep_dirs: bool,
    sizes: &mut [u64],
    folders: &mut u64,
    visits: &mut usize,
    pending: &mut Vec<(NodeId, u64)>,
) {
    let mut accounted = 0u64;
    for child in tree.children(id) {
        if *visits >= MAX_VISITS {
            break;
        }
        *visits += 1;
        let size = tree.allocated(child);
        if size == 0 {
            continue;
        }
        let name = tree.name(child);
        if tree.kind(child) == NodeKind::Directory {
            if let Some(kind) = FileType::of_bundle(name) {
                sizes[kind_index(kind)] += size;
                accounted += size;
            } else if keep_dirs {
                pending.push((child, size));
                accounted += size;
            } else {
                *folders += size;
                accounted += size;
            }
        } else {
            sizes[kind_index(FileType::of_file(name))] += size;
            accounted += size;
        }
    }
    if *visits >= MAX_VISITS {
        *folders += tree.allocated(id).saturating_sub(accounted);
    }
}

fn kind_index(kind: FileType) -> usize {
    FileType::ALL
        .iter()
        .position(|known| *known == kind)
        .unwrap_or(FileType::ALL.len() - 1)
}

fn bucket_order(bucket: Bucket) -> u8 {
    match bucket {
        Bucket::Kind(kind) => kind_index(kind) as u8,
        Bucket::Folders => u8::MAX,
    }
}
