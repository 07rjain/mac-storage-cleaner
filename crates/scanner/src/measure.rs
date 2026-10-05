//! What deleting a set of items would free, measured on disk rather than taken from a scan.
//!
//! Unlike the scan, this asks APFS for each file's private size, so data shared with a clone
//! outside the items isn't promised, and a hard-linked file only counts if every one of its
//! links is among the items.

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};

use crate::sys::{self, Attributes, ObjectType, RawEntry};
use crate::walk::child_path;

const BUFFER_SIZE: usize = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measurement {
    /// Bytes that deleting every item would return to the volume.
    pub freeable: u64,
    /// Bytes the items occupy, including data they share with clones or other hard links.
    pub allocated: u64,
    pub files: u64,
    /// Directories that couldn't be listed; what is inside them isn't counted.
    pub unreadable: u64,
    /// Items that no longer exist.
    pub missing: u64,
}

impl Measurement {
    /// Bytes that stay on disk because clones or hard links elsewhere still use them.
    pub fn shared(&self) -> u64 {
        self.allocated.saturating_sub(self.freeable)
    }
}

/// Measures `roots` and everything inside them. Items inside another root are counted once.
/// Stays on each root's file system and never follows symlinks or downloads dataless files.
pub fn measure(roots: &[PathBuf]) -> io::Result<Measurement> {
    sys::disable_dataless_materialization()?;
    let mut tally = Tally::default();
    let mut directories = Vec::new();
    for root in outermost(roots) {
        let path = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        match sys::read_entry(&path, Attributes::WithPrivateSize) {
            Ok(entry) => {
                if entry.object_type == ObjectType::Directory && !is_dataless(&entry) {
                    directories.push(Job {
                        path,
                        device: entry.device,
                    });
                }
                tally.add(&entry);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => tally.measurement.missing += 1,
            Err(_) => tally.measurement.unreadable += 1,
        }
    }
    if !directories.is_empty() {
        walk(directories, &mut tally);
    }
    Ok(tally.finish())
}

/// Drops every root that is inside another one.
fn outermost(roots: &[PathBuf]) -> Vec<&Path> {
    let mut sorted: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    // Paths sort by component, so everything inside a root comes right after it.
    sorted.sort();
    sorted.dedup();
    let mut kept: Vec<&Path> = Vec::with_capacity(sorted.len());
    for root in sorted {
        if kept.last().is_none_or(|outer| !root.starts_with(outer)) {
            kept.push(root);
        }
    }
    kept
}

struct Job {
    path: CString,
    device: i32,
}

struct Listing {
    job: Job,
    entries: io::Result<Vec<RawEntry>>,
}

/// Lists directories on worker threads; only this thread tallies, so no locking is needed.
fn walk(roots: Vec<Job>, tally: &mut Tally) {
    let threads = std::thread::available_parallelism().map_or(4, |count| count.get());
    let (job_sender, job_receiver) = crossbeam_channel::unbounded::<Job>();
    let (listing_sender, listing_receiver) = crossbeam_channel::unbounded::<Listing>();
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let jobs = job_receiver.clone();
            let listings = listing_sender.clone();
            std::thread::spawn(move || list(jobs, listings))
        })
        .collect();
    drop(job_receiver);
    drop(listing_sender);

    let mut pending = roots.len();
    for job in roots {
        let _ = job_sender.send(job);
    }
    while pending > 0 {
        let Ok(listing) = listing_receiver.recv() else {
            break;
        };
        pending -= 1;
        let Ok(entries) = listing.entries else {
            tally.measurement.unreadable += 1;
            continue;
        };
        for entry in entries {
            tally.add(&entry);
            let other_file_system = entry.mount_status
                & (sys::DIR_MNTSTATUS_MNTPOINT | sys::DIR_MNTSTATUS_TRIGGER)
                != 0
                || entry.device != listing.job.device;
            if entry.object_type == ObjectType::Directory
                && entry.error == 0
                && !is_dataless(&entry)
                && !other_file_system
            {
                pending += 1;
                let _ = job_sender.send(Job {
                    path: child_path(&listing.job.path, &entry.name),
                    device: listing.job.device,
                });
            }
        }
    }
    drop(job_sender);
    for worker in workers {
        let _ = worker.join();
    }
}

fn list(jobs: Receiver<Job>, listings: Sender<Listing>) {
    let mut buffer = vec![0u8; BUFFER_SIZE];
    for job in jobs {
        let entries = sys::open_directory(&job.path).and_then(|directory| {
            let mut entries = Vec::new();
            sys::read_directory(
                &directory,
                &mut buffer,
                &mut entries,
                Attributes::WithPrivateSize,
            )?;
            Ok(entries)
        });
        if listings.send(Listing { job, entries }).is_err() {
            break;
        }
    }
}

fn is_dataless(entry: &RawEntry) -> bool {
    entry.flags & sys::SF_DATALESS != 0
}

#[derive(Default)]
struct Tally {
    measurement: Measurement,
    /// Hard-linked files by device and file ID: (link count, links seen, freeable bytes).
    hard_links: HashMap<(i32, u64), (u32, u32, u64)>,
}

impl Tally {
    fn add(&mut self, entry: &RawEntry) {
        if entry.object_type == ObjectType::File {
            self.measurement.files += 1;
        }
        let freeable = if entry.extended_flags & sys::EF_MAY_SHARE_BLOCKS != 0 {
            entry.private_size.unwrap_or(0).min(entry.allocated_size)
        } else {
            entry.allocated_size
        };
        if entry.object_type == ObjectType::File && entry.link_count > 1 {
            let link = self
                .hard_links
                .entry((entry.device, entry.file_id))
                .or_insert((entry.link_count, 0, freeable));
            link.1 += 1;
            if link.1 == 1 {
                self.measurement.allocated += entry.allocated_size;
            }
            return;
        }
        self.measurement.allocated += entry.allocated_size;
        self.measurement.freeable += freeable;
    }

    fn finish(mut self) -> Measurement {
        for (links, seen, freeable) in self.hard_links.into_values() {
            if seen >= links {
                self.measurement.freeable += freeable;
            }
        }
        self.measurement
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outermost_drops_nested_roots_and_keeps_look_alike_siblings() {
        let roots: Vec<PathBuf> = [
            "/a/b/c", "/ab", "/a", "/a b", "/a/b", "/x/y", "/x/y", "/x/z",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let kept: Vec<&str> = outermost(&roots)
            .into_iter()
            .map(|path| path.to_str().unwrap())
            .collect();
        assert_eq!(kept, ["/a", "/a b", "/ab", "/x/y", "/x/z"]);
    }
}
