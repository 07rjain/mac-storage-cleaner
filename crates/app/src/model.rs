//! What the window shows for a folder, independent of how it is drawn.

use std::cmp::Reverse;

use scanner::{NodeFlags, NodeId, NodeKind, Tree};

/// Something that has a slice in the chart and a row in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Node(NodeId),
    /// macOS, VM, Preboot, Recovery and any other volume in the startup disk's container.
    OtherVolumes,
    /// Data volume space the scan couldn't see: protected folders, snapshots, metadata.
    NotMeasured,
    /// The children of a folder that are too small to draw one by one.
    Smaller(NodeId),
}

/// Slices that make a chart of the scan root add up to the whole startup disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accounting {
    pub other_volumes: u64,
    pub not_measured: u64,
}

impl Accounting {
    /// `data_volume_used` and `container_used` come from the file system; `scanned` from the tree.
    pub fn new(container_used: u64, data_volume_used: u64, scanned: u64) -> Self {
        Self {
            other_volumes: container_used.saturating_sub(data_volume_used),
            not_measured: data_volume_used.saturating_sub(scanned),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub item: Item,
    pub size: u64,
    /// `false` while the size can still grow.
    pub settled: bool,
}

/// Children of `folder`, plus the accounting slices if given, largest first.
pub fn rows(
    tree: &Tree,
    folder: NodeId,
    accounting: Option<Accounting>,
    complete: bool,
) -> Vec<Row> {
    let mut rows: Vec<Row> = tree
        .children(folder)
        .map(|child| Row {
            item: Item::Node(child),
            size: tree.allocated(child),
            settled: tree.is_settled(child),
        })
        .collect();
    if let Some(accounting) = accounting {
        for (item, size) in [
            (Item::OtherVolumes, accounting.other_volumes),
            (Item::NotMeasured, accounting.not_measured),
        ] {
            if size > 0 {
                rows.push(Row {
                    item,
                    size,
                    settled: complete,
                });
            }
        }
    }
    sort_rows(&mut rows);
    rows
}

/// Largest first; ties keep a stable order so colors don't flicker during a scan.
pub fn sort_rows(rows: &mut [Row]) {
    rows.sort_unstable_by_key(|row| (Reverse(row.size), order_key(row.item)));
}

fn order_key(item: Item) -> u64 {
    match item {
        Item::Node(id) => u64::from(id),
        Item::Smaller(_) => u64::MAX - 2,
        Item::OtherVolumes => u64::MAX - 1,
        Item::NotMeasured => u64::MAX,
    }
}

/// Folders from the scan root down to `folder`.
pub fn breadcrumb(tree: &Tree, folder: NodeId) -> Vec<NodeId> {
    let mut path = vec![folder];
    let mut current = folder;
    while let Some(parent) = tree.parent(current) {
        path.push(parent);
        current = parent;
    }
    path.reverse();
    path
}

/// Sub-folders of `folder`, largest first.
pub fn subfolders(tree: &Tree, folder: NodeId) -> Vec<NodeId> {
    let mut folders: Vec<NodeId> = tree
        .children(folder)
        .filter(|&child| tree.kind(child) == NodeKind::Directory)
        .collect();
    folders.sort_unstable_by_key(|&child| (Reverse(tree.allocated(child)), child));
    folders
}

pub fn is_folder(tree: &Tree, item: Item) -> bool {
    matches!(item, Item::Node(id) if tree.kind(id) == NodeKind::Directory)
}

/// A short reason why an item's size may look surprising, for the list.
pub fn note(tree: &Tree, item: Item, complete: bool) -> Option<&'static str> {
    let id = match item {
        Item::Node(id) => id,
        Item::NotMeasured if complete => return Some("Needs Full Disk Access"),
        _ => return None,
    };
    let flags = tree.flags(id);
    [
        (NodeFlags::UNREADABLE, "No access"),
        (NodeFlags::MOUNT_POINT, "Other volume"),
        (NodeFlags::DATALESS, "Stored in iCloud"),
        (NodeFlags::HARD_LINK_DUPLICATE, "Hard link, counted once"),
        (NodeFlags::SHARED_WITH_CLONE, "Clone, counted once"),
    ]
    .into_iter()
    .find(|&(flag, _)| flags.contains(flag))
    .map(|(_, note)| note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_fills_the_container() {
        let accounting = Accounting::new(191, 158, 146);
        assert_eq!(accounting.other_volumes, 33);
        assert_eq!(accounting.not_measured, 12);
        assert_eq!(Accounting::new(100, 90, 95).not_measured, 0);
    }

    #[test]
    fn rows_are_largest_first_with_stable_ties() {
        let mut rows = vec![
            Row {
                item: Item::Node(3),
                size: 10,
                settled: true,
            },
            Row {
                item: Item::NotMeasured,
                size: 50,
                settled: true,
            },
            Row {
                item: Item::Node(1),
                size: 10,
                settled: true,
            },
            Row {
                item: Item::Node(2),
                size: 70,
                settled: true,
            },
        ];
        sort_rows(&mut rows);
        let order: Vec<Item> = rows.iter().map(|row| row.item).collect();
        assert_eq!(
            order,
            [
                Item::Node(2),
                Item::NotMeasured,
                Item::Node(1),
                Item::Node(3)
            ]
        );
    }
}
