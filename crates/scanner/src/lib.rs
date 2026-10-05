//! Fast, read-only directory scanner for macOS.
//!
//! Directories are listed with `getattrlistbulk(2)` on a pool of worker threads, and a single
//! coordinator thread builds a flat arena [`Tree`] that can be read while the scan runs
//! ([`ScanHandle::tree`]). Sizes are allocated (on-disk) bytes.
//! Hard-linked files and unedited APFS clones are counted once; edited clones are counted in
//! full, like `du` does, and reported in [`ScanStats`]. The scan stays on the root's file system, never follows
//! symlinks, and never downloads cloud files: dataless placeholders are recorded but not opened.

mod measure;
mod sys;
mod tree;
mod walk;

use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

pub use measure::{Measurement, measure};
use parking_lot::RwLock;
pub use parking_lot::RwLockReadGuard;
pub use tree::{Children, NodeFlags, NodeId, NodeKind, ScanStats, Tree};

/// The tree of a scan, shareable with other threads. Writers are the scan itself and
/// [`SharedTree::remove`].
#[derive(Clone)]
pub struct SharedTree(Arc<RwLock<Tree>>);

impl SharedTree {
    /// The scan waits while the guard is held, so keep it short.
    pub fn read(&self) -> RwLockReadGuard<'_, Tree> {
        self.0.read()
    }

    /// See [`Tree::remove`].
    pub fn remove(&self, id: NodeId) -> bool {
        self.0.write().remove(id)
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
    /// Worker threads listing directories. Defaults to the number of CPU cores.
    pub threads: Option<NonZeroUsize>,
}

impl ScanOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            threads: None,
        }
    }
}

/// Live counters, safe to read from any thread while a scan runs.
#[derive(Debug, Default)]
pub struct Progress {
    entries: AtomicU64,
    allocated_bytes: AtomicU64,
    pending_directories: AtomicU64,
}

impl Progress {
    pub fn entries(&self) -> u64 {
        self.entries.load(Ordering::Relaxed)
    }

    pub fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes.load(Ordering::Relaxed)
    }

    pub fn pending_directories(&self) -> u64 {
        self.pending_directories.load(Ordering::Relaxed)
    }
}

pub struct ScanHandle {
    progress: Arc<Progress>,
    cancelled: Arc<AtomicBool>,
    tree: Arc<RwLock<Tree>>,
    thread: JoinHandle<()>,
}

impl ScanHandle {
    pub fn progress(&self) -> &Arc<Progress> {
        &self.progress
    }

    /// The tree as scanned so far. The scan waits while the guard is held, so keep it short.
    pub fn tree(&self) -> RwLockReadGuard<'_, Tree> {
        self.tree.read()
    }

    pub fn shared_tree(&self) -> SharedTree {
        SharedTree(Arc::clone(&self.tree))
    }

    /// Stops the scan soon. [`ScanHandle::wait`] then returns a partial tree.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    pub fn wait(self) -> Tree {
        if let Err(panic) = self.thread.join() {
            std::panic::resume_unwind(panic);
        }
        match Arc::try_unwrap(self.tree) {
            Ok(tree) => tree.into_inner(),
            Err(shared) => shared.read().clone(),
        }
    }
}

/// Starts a scan on a background thread.
///
/// Fails if `root` is not a directory, or if the process can't be prevented from downloading
/// dataless (cloud-only) files, since scanning without that guarantee could fill the disk.
pub fn start(options: ScanOptions) -> io::Result<ScanHandle> {
    sys::disable_dataless_materialization()?;

    let metadata = std::fs::symlink_metadata(&options.root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "scan root is not a directory",
        ));
    }

    let threads = options
        .threads
        .or_else(|| std::thread::available_parallelism().ok())
        .map_or(4, NonZeroUsize::get);
    let progress = Arc::new(Progress::default());
    let cancelled = Arc::new(AtomicBool::new(false));

    let tree = Arc::new(RwLock::new(Tree::new(options.root.clone())));
    let walk = walk::Walk {
        root: options.root,
        root_device: metadata.dev() as i32,
        threads,
        progress: Arc::clone(&progress),
        cancelled: Arc::clone(&cancelled),
    };
    let shared = Arc::clone(&tree);
    let thread = std::thread::Builder::new()
        .name("scan-coordinator".into())
        .spawn(move || walk.run(&shared))?;

    Ok(ScanHandle {
        progress,
        cancelled,
        tree,
        thread,
    })
}

/// Scans `options.root` and blocks until done.
pub fn scan(options: ScanOptions) -> io::Result<Tree> {
    Ok(start(options)?.wait())
}
