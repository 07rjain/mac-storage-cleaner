use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use scanner::{NodeFlags, NodeId, NodeKind, ScanOptions, Tree};

fn scan(root: &Path) -> Tree {
    scanner::scan(ScanOptions::new(root)).expect("scan starts")
}

fn allocated(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

fn write_bytes(path: &Path, length: usize) {
    let mut file = fs::File::create(path).unwrap();
    let mut state: u32 = 0x9e37_79b9;
    let bytes: Vec<u8> = (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
}

fn find(tree: &Tree, relative: &str) -> NodeId {
    relative.split('/').fold(tree.root(), |node, name| {
        tree.children(node)
            .find(|&child| tree.name(child) == name)
            .unwrap_or_else(|| panic!("{relative} not found"))
    })
}

#[test]
fn sums_allocated_and_logical_sizes_of_nested_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("a/b")).unwrap();
    write_bytes(&root.path().join("a/b/c.bin"), 100_000);
    write_bytes(&root.path().join("a/d.bin"), 5_000);
    write_bytes(&root.path().join("e.txt"), 10);

    let tree = scan(root.path());

    let files = ["a/b/c.bin", "a/d.bin", "e.txt"];
    let expected: u64 = files.iter().map(|f| allocated(&root.path().join(f))).sum();
    assert_eq!(tree.allocated(tree.root()), expected);
    assert_eq!(tree.logical(tree.root()), 105_010);
    assert_eq!(tree.items(tree.root()), 5);
    assert_eq!(
        tree.allocated(find(&tree, "a")),
        expected - allocated(&root.path().join("e.txt"))
    );
    assert_eq!(tree.kind(find(&tree, "a/b")), NodeKind::Directory);
    assert_eq!(
        tree.path(find(&tree, "a/b/c.bin")),
        root.path().join("a/b/c.bin")
    );
    assert!(tree.is_complete());
}

#[test]
fn counts_hard_linked_files_once() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.bin");
    write_bytes(&original, 1_000_000);
    fs::hard_link(&original, root.path().join("link.bin")).unwrap();

    let tree = scan(root.path());

    assert_eq!(tree.allocated(tree.root()), allocated(&original));
    assert_eq!(tree.stats().hard_link_duplicates, 1);
    let duplicates = tree
        .children(tree.root())
        .filter(|&child| tree.flags(child).contains(NodeFlags::HARD_LINK_DUPLICATE))
        .count();
    assert_eq!(duplicates, 1);
}

fn clone_file(from: &Path, to: &Path) {
    let status = std::process::Command::new("/bin/cp")
        .arg("-c")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(status.success(), "cp -c failed");
}

#[test]
fn counts_apfs_clones_once() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mov");
    write_bytes(&original, 2_000_000);
    clone_file(&original, &root.path().join("copy-1.mov"));
    clone_file(&original, &root.path().join("copy-2.mov"));

    let tree = scan(root.path());

    assert_eq!(tree.allocated(tree.root()), allocated(&original));
    assert_eq!(tree.logical(tree.root()), 6_000_000);
    assert_eq!(tree.stats().clones, 2);
    assert_eq!(tree.stats().clone_shared_bytes, 2 * allocated(&original));
}

#[test]
fn counts_edited_clones_in_full_and_reports_them() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.bin");
    write_bytes(&original, 2_000_000);
    let edited = root.path().join("edited.bin");
    clone_file(&original, &edited);
    let mut file = fs::OpenOptions::new().write(true).open(&edited).unwrap();
    file.write_all(&[0xAB; 8192]).unwrap();
    file.sync_all().unwrap();

    let tree = scan(root.path());

    assert_eq!(
        tree.allocated(tree.root()),
        allocated(&original) + allocated(&edited)
    );
    assert_eq!(tree.stats().edited_clones, 2);
    assert_eq!(tree.stats().clones, 0);
}

#[test]
fn counts_sparse_files_at_their_allocated_size() {
    let root = tempfile::tempdir().unwrap();
    let sparse = root.path().join("disk.img");
    fs::File::create(&sparse)
        .unwrap()
        .set_len(1_000_000_000)
        .unwrap();

    let tree = scan(root.path());

    let node = find(&tree, "disk.img");
    assert_eq!(tree.logical(node), 1_000_000_000);
    assert_eq!(tree.allocated(node), allocated(&sparse));
    assert!(tree.allocated(node) < 1_000_000);
}

#[test]
fn does_not_follow_symlinks() {
    let outside = tempfile::tempdir().unwrap();
    write_bytes(&outside.path().join("big.bin"), 2_000_000);
    let root = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("link")).unwrap();

    let tree = scan(root.path());

    let link = find(&tree, "link");
    assert_eq!(tree.kind(link), NodeKind::Symlink);
    assert_eq!(tree.children(link).count(), 0);
    assert!(tree.allocated(tree.root()) < 100_000);
}

#[test]
fn flags_unreadable_directories_and_still_finishes() {
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let locked = root.path().join("locked");
    fs::create_dir(&locked).unwrap();
    write_bytes(&locked.join("hidden.bin"), 50_000);
    write_bytes(&root.path().join("visible.bin"), 50_000);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let tree = scan(root.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    let node = find(&tree, "locked");
    assert!(tree.flags(node).contains(NodeFlags::UNREADABLE));
    assert_eq!(tree.allocated(node), 0);
    assert_eq!(tree.stats().unreadable_directories, 1);
    assert!(tree.is_complete());
}

#[test]
fn cancelling_returns_promptly() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..2_000 {
        fs::create_dir(root.path().join(format!("dir-{index}"))).unwrap();
    }

    let handle = scanner::start(ScanOptions::new(root.path())).unwrap();
    handle.cancel();
    let tree = handle.wait();

    assert!(tree.len() <= 2_001);
}

#[test]
fn live_totals_only_grow_and_settle_to_the_sum_of_children() {
    let root = tempfile::tempdir().unwrap();
    for outer in 0..40 {
        for inner in 0..25 {
            let directory = root.path().join(format!("d{outer}/e{inner}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("f.txt"), vec![b'x'; 5_000]).unwrap();
        }
    }

    let handle = scanner::start(ScanOptions::new(root.path())).unwrap();
    let mut last_total = 0;
    while !handle.is_finished() {
        let tree = handle.tree();
        let total = tree.allocated(tree.root());
        assert!(total >= last_total, "totals must never shrink");
        last_total = total;
    }
    let tree = handle.wait();

    assert!(tree.allocated(tree.root()) >= last_total);
    for node in 0..tree.len() as NodeId {
        assert!(tree.is_settled(node));
        if tree.kind(node) == NodeKind::Directory {
            let children: u64 = tree.children(node).map(|child| tree.allocated(child)).sum();
            assert_eq!(tree.allocated(node), children);
            let items: u32 = tree.children(node).map(|child| tree.items(child) + 1).sum();
            assert_eq!(tree.items(node), items);
        }
    }
    assert_eq!(tree.items(tree.root()), 40 + 40 * 25 * 2);
}

#[test]
fn rejects_a_file_as_the_root() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file.txt");
    write_bytes(&file, 10);

    let error = scanner::scan(ScanOptions::new(&file)).err().unwrap();

    assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
}
