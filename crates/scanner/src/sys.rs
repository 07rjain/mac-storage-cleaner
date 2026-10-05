//! Thin wrappers over the macOS system calls the scanner needs.

use std::ffi::{CStr, c_int};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(crate) const SF_DATALESS: u32 = 0x4000_0000;
pub(crate) const UF_COMPRESSED: u32 = 0x0000_0020;
pub(crate) const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;
pub(crate) const DIR_MNTSTATUS_TRIGGER: u32 = 0x0000_0002;
pub(crate) const EF_MAY_SHARE_BLOCKS: u64 = 0x0000_0001;
pub(crate) const EF_SHARES_ALL_BLOCKS: u64 = 0x0000_0040;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
const ATTR_CMNEXT_CLONEID: u32 = 0x0000_0100;
const ATTR_CMNEXT_EXT_FLAGS: u32 = 0x0000_0200;
const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x0000_0020;

const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
const IOPOL_SCOPE_PROCESS: c_int = 0;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

const COMMON_ATTRIBUTES: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | ATTR_CMN_ERROR
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID;
const DIRECTORY_ATTRIBUTES: u32 = libc::ATTR_DIR_MOUNTSTATUS;
const FILE_ATTRIBUTES: u32 =
    libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;
/// Requested in the fork group, which `FSOPT_ATTR_CMN_EXTENDED` reinterprets.
/// `ATTR_CMNEXT_PRIVATESIZE` is only requested for [`Attributes::WithPrivateSize`]: it slows
/// a listing by about 45%.
const EXTENDED_ATTRIBUTES: u32 = ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_EXT_FLAGS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attributes {
    Scan,
    WithPrivateSize,
}

impl Attributes {
    fn list(self) -> libc::attrlist {
        let private = match self {
            Self::Scan => 0,
            Self::WithPrivateSize => ATTR_CMNEXT_PRIVATESIZE,
        };
        libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON_ATTRIBUTES,
            volattr: 0,
            dirattr: DIRECTORY_ATTRIBUTES,
            fileattr: FILE_ATTRIBUTES,
            forkattr: EXTENDED_ATTRIBUTES | private,
        }
    }
}

unsafe extern "C" {
    fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
}

/// Makes every file system call in this process fail with `EDEADLK` instead of downloading a
/// dataless file or directory (Apple TN3150).
pub(crate) fn disable_dataless_materialization() -> io::Result<()> {
    // SAFETY: a system call with constant arguments and no pointers.
    let result = unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug)]
pub(crate) struct RawEntry {
    pub name: Box<[u8]>,
    pub object_type: ObjectType,
    pub device: i32,
    pub file_id: u64,
    pub flags: u32,
    pub mount_status: u32,
    pub link_count: u32,
    pub logical_size: u64,
    pub allocated_size: u64,
    /// Bytes not shared with any clone, if requested and reported.
    pub private_size: Option<u64>,
    /// Same value for files whose data came from the same clone.
    pub clone_id: u64,
    pub extended_flags: u64,
    pub error: u32,
}

pub(crate) fn open_directory(path: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by `open` and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Appends every entry of `directory` to `entries`. `buffer` is scratch space reused per call.
pub(crate) fn read_directory(
    directory: &OwnedFd,
    buffer: &mut [u8],
    entries: &mut Vec<RawEntry>,
    attributes: Attributes,
) -> io::Result<()> {
    let mut attributes = attributes.list();

    loop {
        // SAFETY: `attributes` and `buffer` are valid for the call, and the kernel writes at
        // most `buffer.len()` bytes.
        let count = unsafe {
            libc::getattrlistbulk(
                directory.as_raw_fd(),
                (&raw mut attributes).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                FSOPT_ATTR_CMN_EXTENDED,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(());
        }

        let mut offset = 0usize;
        for _ in 0..count {
            let Some(length) = read_u32(buffer, offset) else {
                break;
            };
            let Some(record) = buffer.get(offset..offset + length as usize) else {
                break;
            };
            if let Some(entry) = parse_entry(record) {
                entries.push(entry);
            }
            offset += length as usize;
        }
    }
}

/// Reads the same attributes as [`read_directory`] for the item at `path` itself, without
/// following a final symlink.
pub(crate) fn read_entry(path: &CStr, attributes: Attributes) -> io::Result<RawEntry> {
    let mut attributes = attributes.list();
    // Only valid for `getattrlistbulk`; errors come back from the call itself here.
    attributes.commonattr &= !ATTR_CMN_ERROR;
    let mut buffer = vec![0u8; 4096];
    // SAFETY: `path` is NUL-terminated, `attributes` and `buffer` are valid for the call, and
    // the kernel writes at most `buffer.len()` bytes.
    let result = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut attributes).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            (FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW as u64) as u32,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let length = read_u32(&buffer, 0).unwrap_or(0) as usize;
    buffer
        .get(..length)
        .and_then(parse_entry)
        .ok_or_else(|| io::Error::other("malformed getattrlist reply"))
}

/// Parses one record. Attributes appear in a fixed order: length, returned attributes, error,
/// then common, directory, file and extended attributes, each group in bit order
/// (`man getattrlistbulk`, `man getattrlist`).
fn parse_entry(record: &[u8]) -> Option<RawEntry> {
    let mut cursor = Cursor {
        data: record,
        position: 4,
    };
    let common = cursor.u32()?;
    let _volume = cursor.u32()?;
    let directory = cursor.u32()?;
    let file = cursor.u32()?;
    let extended = cursor.u32()?;

    let error = if common & ATTR_CMN_ERROR != 0 {
        cursor.u32()?
    } else {
        0
    };

    if common & libc::ATTR_CMN_NAME == 0 {
        return None;
    }
    let reference_start = cursor.position;
    let name_offset = cursor.i32()?;
    let name_length = cursor.u32()? as usize;
    let name_start = reference_start.checked_add_signed(name_offset as isize)?;
    let name = record.get(name_start..name_start.checked_add(name_length)?)?;
    let name = name.strip_suffix(&[0]).unwrap_or(name);

    let device = if common & libc::ATTR_CMN_DEVID != 0 {
        cursor.i32()?
    } else {
        0
    };
    let object_type = if common & libc::ATTR_CMN_OBJTYPE != 0 {
        match cursor.u32()? {
            VREG => ObjectType::File,
            VDIR => ObjectType::Directory,
            VLNK => ObjectType::Symlink,
            _ => ObjectType::Other,
        }
    } else {
        ObjectType::Other
    };
    let flags = if common & libc::ATTR_CMN_FLAGS != 0 {
        cursor.u32()?
    } else {
        0
    };
    let file_id = if common & libc::ATTR_CMN_FILEID != 0 {
        cursor.u64()?
    } else {
        0
    };

    let mount_status = if directory & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        cursor.u32()?
    } else {
        0
    };

    let link_count = if file & libc::ATTR_FILE_LINKCOUNT != 0 {
        cursor.u32()?
    } else {
        1
    };
    let logical_size = if file & libc::ATTR_FILE_TOTALSIZE != 0 {
        cursor.u64()?
    } else {
        0
    };
    let allocated_size = if file & libc::ATTR_FILE_ALLOCSIZE != 0 {
        cursor.u64()?
    } else {
        0
    };

    let private_size = if extended & ATTR_CMNEXT_PRIVATESIZE != 0 {
        Some(cursor.u64()?)
    } else {
        None
    };
    let clone_id = if extended & ATTR_CMNEXT_CLONEID != 0 {
        cursor.u64()?
    } else {
        0
    };
    let extended_flags = if extended & ATTR_CMNEXT_EXT_FLAGS != 0 {
        cursor.u64()?
    } else {
        0
    };

    Some(RawEntry {
        name: name.into(),
        object_type,
        device,
        file_id,
        flags,
        mount_status,
        link_count,
        logical_size,
        allocated_size,
        private_size,
        clone_id,
        extended_flags,
        error,
    })
}

fn read_u32(data: &[u8], position: usize) -> Option<u32> {
    let bytes = data.get(position..position + 4)?;
    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let bytes = self.data.get(self.position..self.position + N)?;
        self.position += N;
        bytes.try_into().ok()
    }

    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_ne_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.take().map(i32::from_ne_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_ne_bytes)
    }
}
