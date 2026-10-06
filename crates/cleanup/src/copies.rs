//! Exact copies that would free private space. Hashing stays off the UI thread: callers run
//! [`CopySearch::step`] on a background executor and drop the search when the user presses Stop.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::macos::fs::MetadataExt as MacMetadata;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use scanner::{NodeFlags, NodeId, NodeKind, Tree};

use crate::basket::BasketItem;
use crate::suggest::Candidate;
use crate::{Places, managed, safety};

const MIN_LOGICAL: u64 = 50 * 1024 * 1024;
const MIN_FREEABLE: u64 = 50 * 1024 * 1024;
const MAX_FILES: usize = 200;
const MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const MAX_TIME: Duration = Duration::from_secs(120);
/// `O_NOFOLLOW` on macOS. Opening a symlink fails instead of reading its target.
const O_NOFOLLOW: i32 = 0x0100;
const SF_DATALESS: u32 = 0x4000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyEnd {
    Finished,
    FileCap,
    ByteCap,
    TimeCap,
    Stopped,
}

impl CopyEnd {
    pub fn note(self) -> Option<&'static str> {
        match self {
            Self::Finished => None,
            Self::FileCap => Some(
                "Stopped early: only the largest files were checked. The search was incomplete.",
            ),
            Self::ByteCap => {
                Some("Stopped early: the read limit was reached. The search was incomplete.")
            }
            Self::TimeCap => {
                Some("Stopped early: the time limit was reached. The search was incomplete.")
            }
            Self::Stopped => Some("Stopped. The search was incomplete."),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CopyReport {
    pub items: Vec<Candidate>,
    pub end: CopyEnd,
    pub checked: usize,
    pub total: usize,
}

impl CopyReport {
    pub fn skipped(&self) -> Vec<String> {
        if self.items.is_empty() && self.end == CopyEnd::Finished {
            return vec!["Nothing to remove".into()];
        }
        self.end
            .note()
            .map(|note| note.to_string())
            .into_iter()
            .collect()
    }

    pub fn progress(checked: usize, total: usize) -> String {
        if total == 0 {
            "Checking large files…".into()
        } else {
            format!("Checking large files… {checked} of {total}")
        }
    }
}

/// What was hashed, so the Trash move can refuse a file that changed or whose kept copy is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyProof {
    pub kept: PathBuf,
    kept_device: u64,
    kept_inode: u64,
    kept_len: u64,
    kept_modified: SystemTime,
    copy_len: u64,
    copy_modified: SystemTime,
}

impl CopyProof {
    pub fn check(&self, copy: &Path, basket: &[BasketItem]) -> Result<(), String> {
        if basket
            .iter()
            .any(|item| item.path == self.kept || self.kept.starts_with(&item.path))
        {
            return Err("The file you're keeping is also set to be removed.".into());
        }
        if basket
            .iter()
            .any(|item| copy.starts_with(&item.path) && copy != item.path)
        {
            return Err("This copy is inside a folder already being removed.".into());
        }
        match file_meta(copy) {
            Ok(meta) if meta.len == self.copy_len && meta.modified == self.copy_modified => {}
            Ok(_) => return Err("The copy changed after it was checked.".into()),
            Err(_) => return Err("The copy changed after it was checked.".into()),
        }
        match file_meta(&self.kept) {
            Ok(meta)
                if meta.device == self.kept_device
                    && meta.inode == self.kept_inode
                    && meta.len == self.kept_len
                    && meta.modified == self.kept_modified => {}
            Ok(_) => return Err("The file you're keeping changed.".into()),
            Err(_) => return Err("The file you're keeping is no longer there.".into()),
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Pending {
    node: NodeId,
    path: PathBuf,
    logical: u64,
}

struct Hashed {
    pending: Pending,
    meta: FileMeta,
    digest: [u8; 32],
}

#[derive(Clone, Copy)]
struct FileMeta {
    device: u64,
    inode: u64,
    len: u64,
    modified: SystemTime,
    dataless: bool,
}

pub struct CopySearch {
    files: Vec<Pending>,
    index: usize,
    hashed: Vec<Hashed>,
    bytes: u64,
    end: CopyEnd,
    started: Instant,
    /// A hash that was started and then stopped is left out of `hashed`.
    abandoned: bool,
}

impl CopySearch {
    /// Collects candidates from `tree`. Does not read file contents.
    pub fn start(
        tree: &Tree,
        places: &Places,
        claimed: &std::collections::HashSet<NodeId>,
    ) -> Self {
        let mut files = Vec::new();
        let mut stack = vec![tree.root()];
        while let Some(id) = stack.pop() {
            let flags = tree.flags(id);
            if flags.contains(NodeFlags::REMOVED) || flags.contains(NodeFlags::MOUNT_POINT) {
                continue;
            }
            match tree.kind(id) {
                NodeKind::Directory => {
                    let name = tree.name(id);
                    if name == ".git" {
                        continue;
                    }
                    stack.extend(tree.children(id));
                }
                NodeKind::File
                    if !flags.contains(NodeFlags::DATALESS)
                        && tree.logical(id) >= MIN_LOGICAL
                        && !ancestor_claimed(tree, id, claimed) =>
                {
                    let path = Places::user_path(&tree.path(id));
                    if path_allowed(&path, places) {
                        files.push(Pending {
                            node: id,
                            path,
                            logical: tree.logical(id),
                        });
                    }
                }
                _ => {}
            }
        }
        files.sort_by_key(|file| std::cmp::Reverse(file.logical));
        let end = if files.len() > MAX_FILES {
            files.truncate(MAX_FILES);
            CopyEnd::FileCap
        } else {
            CopyEnd::Finished
        };
        Self {
            files,
            index: 0,
            hashed: Vec::new(),
            bytes: 0,
            end,
            started: Instant::now(),
            abandoned: false,
        }
    }

    pub fn total(&self) -> usize {
        self.files.len()
    }

    pub fn checked(&self) -> usize {
        self.index
    }

    /// Hashes the next file. Returns false when the search should stop.
    pub fn step(&mut self, stop: &AtomicBool) -> bool {
        if self.abandoned || self.index >= self.files.len() {
            return false;
        }
        if stop.load(Ordering::Relaxed) {
            self.end = CopyEnd::Stopped;
            self.abandoned = true;
            return false;
        }
        if self.started.elapsed() >= MAX_TIME {
            self.end = CopyEnd::TimeCap;
            return false;
        }
        if self.bytes >= MAX_BYTES {
            self.end = CopyEnd::ByteCap;
            return false;
        }
        let pending = self.files[self.index].clone();
        self.index += 1;
        match hash_file(&pending.path, &mut self.bytes, stop, self.started) {
            HashRead::Done(meta, digest) => {
                self.hashed.push(Hashed {
                    pending,
                    meta,
                    digest,
                });
                self.index < self.files.len()
            }
            HashRead::Stop => {
                self.end = CopyEnd::Stopped;
                self.abandoned = true;
                false
            }
            HashRead::ByteCap => {
                self.end = CopyEnd::ByteCap;
                false
            }
            HashRead::TimeCap => {
                self.end = CopyEnd::TimeCap;
                false
            }
            HashRead::Skip => self.index < self.files.len(),
        }
    }

    pub fn finish(self) -> CopyReport {
        let checked = self.index;
        let total = self.files.len();
        let end = self.end;
        let mut by_size: HashMap<u64, Vec<Hashed>> = HashMap::new();
        for hashed in self.hashed {
            by_size.entry(hashed.meta.len).or_default().push(hashed);
        }
        let mut items = Vec::new();
        for group in by_size.into_values() {
            if group.len() < 2 {
                continue;
            }
            let mut digests: HashMap<[u8; 32], Vec<Hashed>> = HashMap::new();
            for hashed in group {
                digests.entry(hashed.digest).or_default().push(hashed);
            }
            for same in digests.into_values() {
                if same.len() < 2 {
                    continue;
                }
                items.extend(suggest_group(same));
            }
        }
        items.sort_by_key(|item| std::cmp::Reverse(item.size));
        CopyReport {
            items,
            end,
            checked,
            total,
        }
    }
}

fn suggest_group(mut same: Vec<Hashed>) -> Vec<Candidate> {
    same.sort_by(|left, right| {
        right.meta.modified.cmp(&left.meta.modified).then_with(|| {
            left.pending
                .path
                .as_os_str()
                .cmp(right.pending.path.as_os_str())
        })
    });
    let kept_path = same[0].pending.path.clone();
    let kept_meta = same[0].meta;
    let same_date = same
        .iter()
        .all(|file| file.meta.modified == kept_meta.modified);
    let sentence = if same_date {
        "Same date. This path is kept."
    } else {
        "Identical contents. The newer file is kept."
    };
    let mut items = Vec::new();
    for copy in same.into_iter().skip(1) {
        let Ok(measured) = scanner::measure(std::slice::from_ref(&copy.pending.path)) else {
            continue;
        };
        if measured.freeable < MIN_FREEABLE {
            continue;
        }
        let note = format!("{sentence} Kept: {}", kept_path.display());
        items.push(Candidate {
            node: copy.pending.node,
            path: copy.pending.path,
            size: measured.freeable,
            note: Some(note),
            proof: None,
            copy: Some(CopyProof {
                kept: kept_path.clone(),
                kept_device: kept_meta.device,
                kept_inode: kept_meta.inode,
                kept_len: kept_meta.len,
                kept_modified: kept_meta.modified,
                copy_len: copy.meta.len,
                copy_modified: copy.meta.modified,
            }),
        });
    }
    items
}

fn ancestor_claimed(tree: &Tree, id: NodeId, claimed: &std::collections::HashSet<NodeId>) -> bool {
    let mut current = Some(id);
    while let Some(node) = current {
        if claimed.contains(&node) {
            return true;
        }
        current = tree.parent(node);
    }
    false
}

fn path_allowed(path: &Path, places: &Places) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return false;
    }
    if safety::check_path(path, places).is_err() {
        return false;
    }
    !managed::removal_includes_managed(path, places)
}

enum HashRead {
    Done(FileMeta, [u8; 32]),
    Stop,
    ByteCap,
    TimeCap,
    Skip,
}

fn hash_file(path: &Path, bytes: &mut u64, stop: &AtomicBool, started: Instant) -> HashRead {
    let Ok(before) = file_meta(path) else {
        return HashRead::Skip;
    };
    if before.dataless {
        return HashRead::Skip;
    }
    let mut file = match File::options()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => return HashRead::Skip,
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if stop.load(Ordering::Relaxed) {
            return HashRead::Stop;
        }
        if started.elapsed() >= MAX_TIME {
            return HashRead::TimeCap;
        }
        if *bytes >= MAX_BYTES {
            return HashRead::ByteCap;
        }
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
                *bytes += read as u64;
            }
            Err(_) => return HashRead::Skip,
        }
    }
    let Ok(after) = file_meta(path) else {
        return HashRead::Skip;
    };
    if after.device != before.device
        || after.inode != before.inode
        || after.len != before.len
        || after.modified != before.modified
    {
        return HashRead::Skip;
    }
    HashRead::Done(after, hasher.finish())
}

fn file_meta(path: &Path) -> std::io::Result<FileMeta> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a file",
        ));
    }
    Ok(FileMeta {
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        dataless: MacMetadata::st_flags(&metadata) & SF_DATALESS != 0,
    })
}

#[derive(Clone, Copy)]
struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    filled: usize,
    bits: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            filled: 0,
            bits: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64).wrapping_mul(8));
        if self.filled > 0 {
            let need = 64 - self.filled;
            if data.len() < need {
                self.buffer[self.filled..self.filled + data.len()].copy_from_slice(data);
                self.filled += data.len();
                return;
            }
            self.buffer[self.filled..].copy_from_slice(&data[..need]);
            let block = self.buffer;
            self.compress(&block);
            data = &data[need..];
            self.filled = 0;
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        self.buffer[..data.len()].copy_from_slice(data);
        self.filled = data.len();
    }

    fn finish(mut self) -> [u8; 32] {
        let filled = self.filled;
        self.buffer[filled] = 0x80;
        if filled + 1 > 56 {
            for byte in &mut self.buffer[filled + 1..] {
                *byte = 0;
            }
            let block = self.buffer;
            self.compress(&block);
            self.buffer = [0; 64];
        } else {
            for byte in &mut self.buffer[filled + 1..56] {
                *byte = 0;
            }
        }
        self.buffer[56..].copy_from_slice(&self.bits.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);
        let mut out = [0u8; 32];
        for (slot, word) in self.state.iter().enumerate() {
            out[slot * 4..][..4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for index in 0..16 {
            w[index] = u32::from_be_bytes(block[index * 4..][..4].try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Places;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn package_and_git_paths_are_not_read() {
        let places = Places {
            home: std::path::PathBuf::from("/Users/test"),
        };
        assert!(!path_allowed(
            std::path::Path::new("/Users/test/Apps/Foo.app/Contents/MacOS/big.bin"),
            &places
        ));
        assert!(!path_allowed(
            std::path::Path::new("/Users/test/code/.git/objects/big.bin"),
            &places
        ));
    }

    #[test]
    fn sha256_matches_the_empty_and_abc_digests() {
        let empty = Sha256::new().finish();
        assert_eq!(
            empty,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55
            ]
        );
        let mut abc = Sha256::new();
        abc.update(b"abc");
        assert_eq!(
            abc.finish(),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
    }

    #[test]
    fn stop_drops_the_hash_in_progress() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("big.bin");
        std::fs::write(&path, vec![1u8; MIN_LOGICAL as usize + 8]).unwrap();
        let tree = scanner::scan(scanner::ScanOptions::new(root.path())).unwrap();
        let places = Places {
            home: root.path().to_path_buf(),
        };
        let mut search = CopySearch::start(&tree, &places, &HashSet::new());
        let stop = AtomicBool::new(true);
        assert!(!search.step(&stop));
        let report = search.finish();
        assert_eq!(report.end, CopyEnd::Stopped);
        assert!(report.items.is_empty());
        assert!(
            report
                .skipped()
                .iter()
                .any(|line| line.contains("incomplete"))
        );
    }

    fn fill(path: &std::path::Path, byte: u8) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, vec![byte; (MIN_LOGICAL as usize) + 4096]).unwrap();
    }

    fn search(root: &std::path::Path) -> CopyReport {
        let tree = scanner::scan(scanner::ScanOptions::new(root)).unwrap();
        let places = Places {
            home: root.to_path_buf(),
        };
        let mut search = CopySearch::start(&tree, &places, &HashSet::new());
        let stop = AtomicBool::new(false);
        while search.step(&stop) {}
        search.finish()
    }

    #[test]
    fn an_independent_copy_is_suggested_and_a_changed_copy_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let older = root.path().join("files/older.bin");
        let newer = root.path().join("files/newer.bin");
        fill(&older, 9);
        fill(&newer, 9);
        let old_time = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::open(&older)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        let report = search(root.path());
        assert_eq!(report.end, CopyEnd::Finished);
        assert_eq!(
            report.items.len(),
            1,
            "{:?}",
            report
                .items
                .iter()
                .map(|item| &item.path)
                .collect::<Vec<_>>()
        );
        assert!(report.items[0].path.ends_with("older.bin"));
        assert!(report.items[0].size >= MIN_FREEABLE);
        assert!(
            report.items[0]
                .note
                .as_deref()
                .unwrap()
                .contains("newer file is kept")
        );

        let places = Places {
            home: root.path().to_path_buf(),
        };
        let proof = report.items[0].copy.clone().unwrap();
        let mut basket = crate::Basket::new(places.clone(), root.path());
        basket
            .add_copy(
                &older,
                Some(report.items[0].node),
                report.items[0].size,
                &proof,
            )
            .unwrap();
        std::fs::write(&older, vec![3u8; 64]).unwrap();
        let changed = crate::move_to_trash(
            basket.items(),
            &places,
            root.path(),
            &crate::RunningApps::default(),
            &crate::Inventory::known(Vec::<String>::new()),
        );
        assert_eq!(changed.moved.len(), 0);
        assert!(!changed.failed.is_empty());
        std::fs::write(&older, vec![9u8; (MIN_LOGICAL as usize) + 4096]).unwrap();
        std::fs::File::open(&older)
            .unwrap()
            .set_modified(old_time)
            .unwrap();
        let mut basket = crate::Basket::new(places.clone(), root.path());
        basket
            .add_copy(
                &older,
                Some(report.items[0].node),
                report.items[0].size,
                &proof,
            )
            .unwrap();
        basket
            .add(&newer, None, crate::Category::Chosen, 1)
            .unwrap();
        let outcome = crate::move_to_trash(
            basket.items(),
            &places,
            root.path(),
            &crate::RunningApps::default(),
            &crate::Inventory::known(Vec::<String>::new()),
        );
        assert!(
            outcome
                .failed
                .iter()
                .any(|failed| failed.path.ends_with("older.bin")),
            "keeping the newer file in the basket refuses the copy"
        );
        assert!(older.exists());
    }

    #[test]
    fn a_clone_does_not_count_as_freeable_space() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("files/original.bin");
        let clone = root.path().join("files/clone.bin");
        fill(&original, 4);
        let copied = std::process::Command::new("/bin/cp")
            .args(["-c"])
            .arg(&original)
            .arg(&clone)
            .status()
            .unwrap();
        assert!(copied.success(), "clonefile copy failed");
        let report = search(root.path());
        assert!(
            report.items.is_empty(),
            "an unedited clone must not be offered"
        );
        assert_eq!(report.skipped(), ["Nothing to remove".to_string()]);
    }
}
