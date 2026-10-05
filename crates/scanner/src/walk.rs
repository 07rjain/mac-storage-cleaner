use std::collections::HashSet;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::RwLock;

use crate::Progress;
use crate::sys::{self, ObjectType, RawEntry};
use crate::tree::{NodeFlags, NodeId, NodeKind, Totals, Tree};

const BUFFER_SIZE: usize = 256 * 1024;
/// Listings waiting for the coordinator. Bounded so fast workers can't pile up memory.
const LISTING_QUEUE: usize = 64;

pub(crate) struct Walk {
    pub root: PathBuf,
    pub root_device: i32,
    pub threads: usize,
    pub progress: Arc<Progress>,
    pub cancelled: Arc<AtomicBool>,
}

struct Job {
    node: NodeId,
    path: CString,
}

/// Data already counted once: hard-linked files by file ID, APFS clones by clone ID.
#[derive(Default)]
struct Seen {
    hard_links: HashSet<u64>,
    clones: HashSet<u64>,
}

struct Listing {
    node: NodeId,
    path: CString,
    entries: io::Result<Vec<RawEntry>>,
}

impl Walk {
    /// Workers only make system calls; this thread is the only one that writes the tree.
    pub(crate) fn run(self, tree: &RwLock<Tree>) {
        // Only the coordinator sends jobs and never blocks doing so, so a bounded listing
        // queue can't deadlock.
        let (job_sender, job_receiver) = crossbeam_channel::unbounded::<Job>();
        let (listing_sender, listing_receiver) =
            crossbeam_channel::bounded::<Listing>(LISTING_QUEUE);

        let workers: Vec<_> = (0..self.threads)
            .map(|index| {
                let jobs = job_receiver.clone();
                let listings = listing_sender.clone();
                let cancelled = Arc::clone(&self.cancelled);
                std::thread::Builder::new()
                    .name(format!("scan-worker-{index}"))
                    .spawn(move || list_directories(jobs, listings, cancelled))
                    .expect("failed to spawn scan worker")
            })
            .collect();
        drop(job_receiver);
        drop(listing_sender);

        let complete = self.coordinate(tree, job_sender, &listing_receiver);
        drop(listing_receiver);
        for worker in workers {
            let _ = worker.join();
        }
        tree.write().finish(complete);
    }

    fn coordinate(
        &self,
        tree: &RwLock<Tree>,
        jobs: Sender<Job>,
        listings: &Receiver<Listing>,
    ) -> bool {
        let root_path =
            CString::new(self.root.as_os_str().as_bytes()).expect("paths can't contain NUL bytes");
        let mut pending: u64 = 1;
        let _ = jobs.send(Job {
            node: tree.read().root(),
            path: root_path,
        });
        let mut seen = Seen::default();

        while pending > 0 {
            if self.cancelled.load(Ordering::Relaxed) {
                return false;
            }
            let Ok(listing) = listings.recv() else {
                return false;
            };
            pending -= 1;

            let mut tree = tree.write();
            let mut totals = Totals::default();
            let mut new_jobs = 0;
            match listing.entries {
                Ok(entries) => {
                    for entry in entries {
                        let (node, job) =
                            self.insert(&mut tree, listing.node, &listing.path, entry, &mut seen);
                        totals.allocated += tree.allocated(node);
                        totals.logical += tree.logical(node);
                        totals.items += 1;
                        if let Some(job) = job {
                            tree.mark_pending(node);
                            new_jobs += 1;
                            let _ = jobs.send(job);
                        }
                    }
                }
                Err(error) => record_listing_error(&mut tree, listing.node, &error),
            }
            tree.finish_listing(listing.node, totals, new_jobs);
            pending += u64::from(new_jobs);

            self.progress
                .entries
                .store(tree.stats().entries(), Ordering::Relaxed);
            self.progress
                .pending_directories
                .store(pending, Ordering::Relaxed);
        }
        true
    }

    /// Inserts `entry` under `parent`, and returns a job too if it should be descended into.
    fn insert(
        &self,
        tree: &mut Tree,
        parent: NodeId,
        parent_path: &CString,
        entry: RawEntry,
        seen: &mut Seen,
    ) -> (NodeId, Option<Job>) {
        let kind = match entry.object_type {
            ObjectType::File => NodeKind::File,
            ObjectType::Directory => NodeKind::Directory,
            ObjectType::Symlink => NodeKind::Symlink,
            ObjectType::Other => NodeKind::Other,
        };
        let stats = tree.stats_mut();
        match kind {
            NodeKind::File => stats.files += 1,
            NodeKind::Directory => stats.directories += 1,
            NodeKind::Symlink => stats.symlinks += 1,
            NodeKind::Other => stats.other += 1,
        }

        let mut flags = NodeFlags::default();
        let mut allocated = entry.allocated_size;
        let mut logical = entry.logical_size;

        if entry.error != 0 {
            flags |= NodeFlags::ENTRY_ERROR;
            stats.entry_errors += 1;
        }
        let dataless = entry.flags & sys::SF_DATALESS != 0;
        if dataless {
            flags |= NodeFlags::DATALESS;
            stats.dataless_items += 1;
        }
        if entry.flags & sys::UF_COMPRESSED != 0 {
            flags |= NodeFlags::COMPRESSED;
        }
        if kind == NodeKind::File && entry.link_count > 1 && !seen.hard_links.insert(entry.file_id)
        {
            flags |= NodeFlags::HARD_LINK_DUPLICATE;
            stats.hard_link_duplicates += 1;
            allocated = 0;
            logical = 0;
        } else if kind == NodeKind::File && entry.extended_flags & sys::EF_MAY_SHARE_BLOCKS != 0 {
            // Unedited clones share one clone ID, so the first one seen carries the shared data.
            // An edited clone gets a new ID and can't be matched to the file it shares blocks
            // with, so it is counted in full, as `du` and Finder do.
            if entry.extended_flags & sys::EF_SHARES_ALL_BLOCKS == 0 {
                stats.edited_clones += 1;
                stats.edited_clone_bytes += allocated;
            } else if !seen.clones.insert(entry.clone_id) {
                flags |= NodeFlags::SHARED_WITH_CLONE;
                stats.clones += 1;
                stats.clone_shared_bytes += allocated;
                allocated = 0;
            }
        }

        let other_file_system =
            entry.mount_status & (sys::DIR_MNTSTATUS_MNTPOINT | sys::DIR_MNTSTATUS_TRIGGER) != 0
                || entry.device != self.root_device;
        let descend =
            kind == NodeKind::Directory && entry.error == 0 && !dataless && !other_file_system;
        if kind == NodeKind::Directory && other_file_system {
            flags |= NodeFlags::MOUNT_POINT;
            stats.mount_points_skipped += 1;
        }

        self.progress
            .allocated_bytes
            .fetch_add(allocated, Ordering::Relaxed);
        let node = tree.insert(parent, &entry.name, kind, flags, allocated, logical);

        let job = descend.then(|| Job {
            node,
            path: child_path(parent_path, &entry.name),
        });
        (node, job)
    }
}

fn list_directories(jobs: Receiver<Job>, listings: Sender<Listing>, cancelled: Arc<AtomicBool>) {
    let mut buffer = vec![0u8; BUFFER_SIZE];
    for job in jobs {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let entries = sys::open_directory(&job.path).and_then(|directory| {
            let mut entries = Vec::new();
            sys::read_directory(&directory, &mut buffer, &mut entries)?;
            Ok(entries)
        });
        let listing = Listing {
            node: job.node,
            path: job.path,
            entries,
        };
        if listings.send(listing).is_err() {
            break;
        }
    }
}

fn record_listing_error(tree: &mut Tree, node: NodeId, error: &io::Error) {
    match error.raw_os_error() {
        Some(libc::EDEADLK) => {
            tree.add_flags(node, NodeFlags::DATALESS);
            tree.stats_mut().dataless_items += 1;
        }
        Some(libc::ENOENT | libc::ENOTDIR) => {
            tree.add_flags(node, NodeFlags::ENTRY_ERROR);
            tree.stats_mut().entry_errors += 1;
        }
        _ => {
            tree.add_flags(node, NodeFlags::UNREADABLE);
            tree.stats_mut().unreadable_directories += 1;
        }
    }
}

fn child_path(parent: &CString, name: &[u8]) -> CString {
    let parent = parent.as_bytes();
    let mut path = Vec::with_capacity(parent.len() + 1 + name.len() + 1);
    path.extend_from_slice(parent);
    if parent.last() != Some(&b'/') {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    CString::new(path).expect("file names can't contain NUL bytes")
}
