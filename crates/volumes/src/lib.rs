//! Volume capacity, usage, purgeable space and local snapshots for macOS.

mod foundation;

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The writable half of the startup disk. Scanning it directly avoids counting firmlinks twice.
pub const DATA_VOLUME_MOUNT_POINT: &str = "/System/Volumes/Data";

#[derive(Debug, Clone)]
pub struct Volume {
    pub mount_point: PathBuf,
    /// BSD device, for example `/dev/disk3s5`.
    pub device: String,
    pub file_system: String,
    /// Size of the volume, or of its whole container for APFS.
    pub capacity: u64,
    /// Free space available to this volume, shared with the rest of an APFS container.
    pub available: u64,
    /// Bytes used by this volume alone.
    pub used: u64,
    flags: u32,
}

impl Volume {
    /// APFS container the volume lives in, for example `disk3` for `/dev/disk3s5`.
    pub fn container(&self) -> Option<&str> {
        let name = self.device.strip_prefix("/dev/")?;
        let digits = name.strip_prefix("disk")?;
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        (end > 0).then(|| &name[..4 + end])
    }

    pub fn is_apfs(&self) -> bool {
        self.file_system == "apfs"
    }

    pub fn is_local(&self) -> bool {
        self.flags & libc::MNT_LOCAL as u32 != 0
    }

    /// `false` for system volumes that Finder hides, such as Preboot and VM.
    pub fn is_browsable(&self) -> bool {
        self.flags & libc::MNT_DONTBROWSE as u32 == 0
    }

    pub fn is_read_only(&self) -> bool {
        self.flags & libc::MNT_RDONLY as u32 != 0
    }
}

pub fn mounted_volumes() -> io::Result<Vec<Volume>> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: `getmntinfo` points `mounts` at storage owned by libc, valid until the next call
    // on this thread; it is copied out below before returning.
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: libc returned `count` initialized `statfs` records at `mounts`.
    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };
    Ok(mounts.iter().map(volume_from_statfs).collect())
}

/// The name Finder shows for the volume containing `path`, for example "Macintosh HD".
pub fn volume_name(path: &Path) -> Option<String> {
    foundation::volume_localized_name(path)
}

pub fn volume_at(mount_point: &Path) -> io::Result<Volume> {
    let path = c_path(mount_point)?;
    // SAFETY: `statfs` is plain data, and all-zero is a valid value for it.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is NUL-terminated and `info` is valid for writes.
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(volume_from_statfs(&info))
}

fn volume_from_statfs(info: &libc::statfs) -> Volume {
    let block_size = u64::from(info.f_bsize);
    let mount_point = c_chars_to_string(&info.f_mntonname);
    let statfs_used = info.f_blocks.saturating_sub(info.f_bfree) * block_size;
    let used = c_path(Path::new(&mount_point))
        .ok()
        .and_then(|path| volume_space_used(&path).ok())
        .unwrap_or(statfs_used);
    Volume {
        mount_point: PathBuf::from(mount_point),
        device: c_chars_to_string(&info.f_mntfromname),
        file_system: c_chars_to_string(&info.f_fstypename),
        capacity: info.f_blocks * block_size,
        available: info.f_bavail * block_size,
        used,
        flags: info.f_flags,
    }
}

/// Exact bytes used by one APFS volume (`ATTR_VOL_SPACEUSED`).
fn volume_space_used(mount_point: &CStr) -> io::Result<u64> {
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0u8; 16];
    // SAFETY: `attributes` and `buffer` are valid for the call, and the kernel writes at most
    // `buffer.len()` bytes.
    let result = unsafe {
        libc::getattrlist(
            mount_point.as_ptr(),
            (&raw mut attributes).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let used = i64::from_ne_bytes(buffer[4..12].try_into().expect("8 bytes"));
    u64::try_from(used).map_err(|_| io::Error::other("negative volume usage"))
}

/// Number of local Time Machine snapshots on the volume at `mount_point`. Their size is not
/// exposed by macOS, so none is reported.
pub fn local_snapshot_count(mount_point: &Path) -> Option<usize> {
    let output = Command::new("/usr/bin/tmutil")
        .arg("listlocalsnapshots")
        .arg(mount_point)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Some(
        listing
            .lines()
            .filter(|line| line.trim_start().starts_with("com.apple.TimeMachine."))
            .count(),
    )
}

/// The startup disk's APFS container, seen from its Data volume.
#[derive(Debug, Clone)]
pub struct StartupDisk {
    pub data_volume: Volume,
    /// Space macOS can free on demand (caches, snapshots and similar), if reported.
    pub purgeable: Option<u64>,
    pub local_snapshots: Option<usize>,
}

impl StartupDisk {
    pub fn read() -> io::Result<Self> {
        let data_volume = volume_at(Path::new(DATA_VOLUME_MOUNT_POINT))?;
        let purgeable =
            foundation::available_capacity_for_important_usage(Path::new(DATA_VOLUME_MOUNT_POINT))
                .map(|important| important.saturating_sub(data_volume.available));
        let local_snapshots = local_snapshot_count(Path::new("/"));
        Ok(Self {
            data_volume,
            purgeable,
            local_snapshots,
        })
    }

    pub fn capacity(&self) -> u64 {
        self.data_volume.capacity
    }

    pub fn available(&self) -> u64 {
        self.data_volume.available
    }

    /// Everything stored in the container, across all of its volumes.
    pub fn used(&self) -> u64 {
        self.capacity().saturating_sub(self.available())
    }
}

/// Splits the container's used space so that a scan of the Data volume adds up exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accounting {
    pub container_used: u64,
    /// Bytes found by the scan.
    pub scanned: u64,
    /// Data volume bytes the scan could not see: snapshots, protected folders, and metadata.
    pub not_scanned: u64,
    /// macOS, VM, Preboot, Recovery and other volumes in the container.
    pub other_volumes: u64,
    /// Bytes the scan counted beyond the Data volume's usage, for example from APFS clones,
    /// which are counted in full. Zero when the numbers agree.
    pub overcount: u64,
}

impl Accounting {
    pub fn new(disk: &StartupDisk, scanned: u64) -> Self {
        let data_used = disk.data_volume.used;
        let container_used = disk.used();
        Self {
            container_used,
            scanned,
            not_scanned: data_used.saturating_sub(scanned),
            other_volumes: container_used.saturating_sub(data_used),
            overcount: scanned.saturating_sub(data_used),
        }
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn c_chars_to_string(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(device: &str) -> Volume {
        Volume {
            mount_point: PathBuf::from("/"),
            device: device.into(),
            file_system: "apfs".into(),
            capacity: 0,
            available: 0,
            used: 0,
            flags: 0,
        }
    }

    #[test]
    fn container_comes_from_the_device_name() {
        assert_eq!(volume("/dev/disk3s5").container(), Some("disk3"));
        assert_eq!(volume("/dev/disk3s1s1").container(), Some("disk3"));
        assert_eq!(volume("/dev/disk12s2").container(), Some("disk12"));
        assert_eq!(volume("map auto_home").container(), None);
    }

    #[test]
    fn accounting_adds_up_to_container_use() {
        let mut data_volume = volume("/dev/disk3s5");
        data_volume.capacity = 1_000;
        data_volume.available = 400;
        data_volume.used = 500;
        let disk = StartupDisk {
            data_volume,
            purgeable: None,
            local_snapshots: None,
        };

        let accounting = Accounting::new(&disk, 420);

        assert_eq!(accounting.container_used, 600);
        assert_eq!(accounting.not_scanned, 80);
        assert_eq!(accounting.other_volumes, 100);
        assert_eq!(accounting.overcount, 0);
        assert_eq!(
            accounting.scanned + accounting.not_scanned + accounting.other_volumes,
            accounting.container_used
        );
    }

    #[test]
    fn reads_the_startup_disk() {
        let disk = StartupDisk::read().expect("startup disk is readable");
        assert!(disk.capacity() > 0);
        assert!(disk.data_volume.used > 0);
        assert!(disk.data_volume.used <= disk.used());
        assert!(disk.data_volume.container().is_some());
    }

    #[test]
    fn names_the_startup_disk() {
        let name = volume_name(Path::new("/")).expect("the startup disk has a name");
        assert!(!name.is_empty());
    }
}
