//! Live folder refresh with FSEvents.
//!
//! The scanner records [`current_event_id`] before a walk. After the walk, [`Watch`] delivers
//! paths that changed from that ID onward. [`refresh_targets`] turns those paths into folders
//! the tree can [`Tree::replace`](crate::Tree::replace).

use std::collections::BTreeSet;
use std::ffi::{CStr, CString, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::tree::{NodeId, NodeKind, Tree};

const USE_CF_TYPES: u32 = 0x0000_0001;
const NO_DEFER: u32 = 0x0000_0002;
const WATCH_ROOT: u32 = 0x0000_0004;
const USER_DROPPED: u32 = 0x0000_0002;
const KERNEL_DROPPED: u32 = 0x0000_0004;
const HISTORY_DONE: u32 = 0x0000_0010;
const ROOT_CHANGED: u32 = 0x0000_0020;
const LATENCY_SECONDS: f64 = 0.75;
const MAX_FOLDERS: usize = 8;

type CFIndex = isize;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFMutableArrayRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type FSEventStreamRef = *mut c_void;
type FSEventStreamEventId = u64;

#[repr(C)]
struct FSEventStreamContext {
    version: CFIndex,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

#[repr(C)]
struct CFArrayCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
    static kCFRunLoopDefaultMode: CFStringRef;

    fn CFStringCreateWithFileSystemRepresentation(
        allocator: *const c_void,
        buffer: *const i8,
    ) -> CFStringRef;
    fn CFStringGetFileSystemRepresentation(
        string: CFStringRef,
        buffer: *mut i8,
        max_buf_len: CFIndex,
    ) -> u8;
    fn CFArrayCreateMutable(
        allocator: *const c_void,
        capacity: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFMutableArrayRef;
    fn CFArrayAppendValue(array: CFMutableArrayRef, value: *const c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRunLoopStop(runloop: CFRunLoopRef);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventsGetCurrentEventId() -> FSEventStreamEventId;
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: unsafe extern "C" fn(
            FSEventStreamRef,
            *mut c_void,
            usize,
            *mut c_void,
            *const u32,
            *const FSEventStreamEventId,
        ),
        context: *mut FSEventStreamContext,
        paths_to_watch: CFArrayRef,
        since_when: FSEventStreamEventId,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamScheduleWithRunLoop(
        stream: FSEventStreamRef,
        runloop: CFRunLoopRef,
        mode: CFStringRef,
    );
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
}

/// The FSEvents counter to pass to [`Watch::start`] so changes during the scan are not missed.
pub fn current_event_id() -> u64 {
    // SAFETY: a scalar system call with no pointers.
    unsafe { FSEventsGetCurrentEventId() }
}

/// Folders the tree should scan again after paths changed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    None,
    /// The scan root changed, or too many folders did; replace the whole tree.
    Root,
    Folders(Vec<NodeId>),
}

/// Paths from one FSEvents callback, with the flags OR'd together.
#[derive(Debug, Clone)]
pub struct EventBatch {
    pub paths: Vec<PathBuf>,
    pub flags: u32,
}

impl EventBatch {
    pub fn history_done(&self) -> bool {
        self.flags & HISTORY_DONE != 0
    }

    /// The kernel dropped events, or the watched folder itself moved.
    pub fn needs_full_scan(&self) -> bool {
        self.flags & (USER_DROPPED | KERNEL_DROPPED | ROOT_CHANGED) != 0
    }
}

struct CallbackState {
    tx: Sender<EventBatch>,
}

/// An FSEventStream on its own run-loop thread. Dropping it stops the stream.
pub struct Watch {
    runloop: CFRunLoopRef,
    thread: Option<JoinHandle<()>>,
    info: *mut CallbackState,
}

// The run-loop thread is the only other user of these pointers, and Drop joins it first.
unsafe impl Send for Watch {}

impl Watch {
    /// Watches `path` for events after `since` (from [`current_event_id`]).
    pub fn start(path: &Path, since: u64) -> io::Result<(Self, Receiver<EventBatch>)> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<(usize, usize)>>();
        let thread = thread::Builder::new()
            .name("fsevents".into())
            .spawn(move || run_loop(c_path, since, tx, ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok((runloop, info))) => Ok((
                Self {
                    runloop: runloop as CFRunLoopRef,
                    thread: Some(thread),
                    info: info as *mut CallbackState,
                },
                rx,
            )),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other("file-events thread exited"))
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if !self.runloop.is_null() {
            // SAFETY: `runloop` is the watcher thread's run loop until that thread exits.
            unsafe { CFRunLoopStop(self.runloop) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if !self.info.is_null() {
            // SAFETY: the watcher thread has exited, so the callback no longer uses `info`.
            drop(unsafe { Box::from_raw(self.info) });
            self.info = ptr::null_mut();
        }
    }
}

fn run_loop(
    path: CString,
    since: u64,
    tx: Sender<EventBatch>,
    ready: Sender<io::Result<(usize, usize)>>,
) {
    let info = Box::into_raw(Box::new(CallbackState { tx }));
    let stream = unsafe { create_stream(&path, since, info) };
    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            drop(unsafe { Box::from_raw(info) });
            let _ = ready.send(Err(error));
            return;
        }
    };
    unsafe {
        let runloop = CFRunLoopGetCurrent();
        FSEventStreamScheduleWithRunLoop(stream, runloop, kCFRunLoopDefaultMode);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            drop(Box::from_raw(info));
            let _ = ready.send(Err(io::Error::other("FSEventStreamStart failed")));
            return;
        }
        let _ = ready.send(Ok((runloop as usize, info as usize)));
        CFRunLoopRun();
        FSEventStreamStop(stream);
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
    }
}

unsafe fn create_stream(
    path: &CString,
    since: u64,
    info: *mut CallbackState,
) -> io::Result<FSEventStreamRef> {
    let cf_path = unsafe { CFStringCreateWithFileSystemRepresentation(ptr::null(), path.as_ptr()) };
    if cf_path.is_null() {
        return Err(io::Error::other(
            "CFStringCreateWithFileSystemRepresentation",
        ));
    }
    let array =
        unsafe { CFArrayCreateMutable(ptr::null(), 1, std::ptr::addr_of!(kCFTypeArrayCallBacks)) };
    if array.is_null() {
        unsafe { CFRelease(cf_path) };
        return Err(io::Error::other("CFArrayCreateMutable"));
    }
    unsafe { CFArrayAppendValue(array, cf_path) };
    unsafe { CFRelease(cf_path) };
    let mut context = FSEventStreamContext {
        version: 0,
        info: info.cast(),
        retain: ptr::null(),
        release: ptr::null(),
        copy_description: ptr::null(),
    };
    let stream = unsafe {
        FSEventStreamCreate(
            ptr::null(),
            callback,
            &raw mut context,
            array as CFArrayRef,
            since,
            LATENCY_SECONDS,
            USE_CF_TYPES | NO_DEFER | WATCH_ROOT,
        )
    };
    unsafe { CFRelease(array as *const c_void) };
    if stream.is_null() {
        return Err(io::Error::other("FSEventStreamCreate"));
    }
    Ok(stream)
}

unsafe extern "C" fn callback(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    event_paths: *mut c_void,
    flags: *const u32,
    _ids: *const FSEventStreamEventId,
) {
    if info.is_null() {
        return;
    }
    let state = unsafe { &*(info as *const CallbackState) };
    let array = event_paths as CFArrayRef;
    let reported = if array.is_null() {
        0
    } else {
        unsafe { CFArrayGetCount(array) as usize }.min(count)
    };
    let mut paths = Vec::with_capacity(reported);
    let mut combined = 0u32;
    for index in 0..reported {
        let flag = unsafe { *flags.add(index) };
        combined |= flag;
        if flag & HISTORY_DONE != 0 {
            continue;
        }
        let string = unsafe { CFArrayGetValueAtIndex(array, index as CFIndex) } as CFStringRef;
        if let Some(path) = cf_path(string) {
            paths.push(path);
        }
    }
    if paths.is_empty() && combined & HISTORY_DONE == 0 {
        return;
    }
    let _ = state.tx.send(EventBatch {
        paths,
        flags: combined,
    });
}

fn cf_path(string: CFStringRef) -> Option<PathBuf> {
    if string.is_null() {
        return None;
    }
    let mut buffer = [0i8; 1024];
    let ok = unsafe {
        CFStringGetFileSystemRepresentation(string, buffer.as_mut_ptr(), buffer.len() as CFIndex)
    };
    if ok == 0 {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(cstr.to_bytes())))
}

/// The folders in `tree` that should be scanned again for `paths`.
pub fn refresh_targets(tree: &Tree, paths: &[PathBuf]) -> Refresh {
    if paths.is_empty() {
        return Refresh::None;
    }
    let mut folders = BTreeSet::new();
    for path in paths {
        match folder_for_path(tree, path) {
            None => {}
            Some(id) if id == tree.root() => return Refresh::Root,
            Some(id) => {
                folders.insert(id);
            }
        }
    }
    coalesce(tree, folders)
}

fn folder_for_path(tree: &Tree, path: &Path) -> Option<NodeId> {
    let root_path = tree.root_path();
    let rest = relative_to_root(root_path, path)?;
    if rest.as_os_str().is_empty() {
        return Some(tree.root());
    }
    let mut current = tree.root();
    for component in rest.components() {
        let name = component.as_os_str();
        match tree
            .children(current)
            .find(|&child| tree.name(child) == name)
        {
            Some(child) if tree.kind(child) == NodeKind::Directory => current = child,
            Some(_) => return Some(current),
            None => return Some(current),
        }
    }
    if tree.kind(current) == NodeKind::Directory {
        Some(current)
    } else {
        tree.parent(current)
    }
}

/// FSEvents reports real paths (`/private/var/...`); the scan root may be the symlink (`/var/...`).
fn relative_to_root(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(rest) = path.strip_prefix(root) {
        return Some(rest.to_path_buf());
    }
    let root = root.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    if path == root {
        return Some(PathBuf::new());
    }
    path.strip_prefix(root).ok().map(|rest| rest.to_path_buf())
}

fn coalesce(tree: &Tree, mut folders: BTreeSet<NodeId>) -> Refresh {
    if folders.is_empty() {
        return Refresh::None;
    }
    if folders.contains(&tree.root()) {
        return Refresh::Root;
    }
    loop {
        let parents: Vec<NodeId> = folders
            .iter()
            .copied()
            .filter(|&id| {
                let mut parent = tree.parent(id);
                while let Some(node) = parent {
                    if folders.contains(&node) {
                        return false;
                    }
                    if node == tree.root() {
                        break;
                    }
                    parent = tree.parent(node);
                }
                true
            })
            .collect();
        folders = parents.into_iter().collect();
        if folders.len() <= MAX_FOLDERS {
            break;
        }
        folders = folders
            .into_iter()
            .map(|id| tree.parent(id).unwrap_or(tree.root()))
            .collect();
        if folders.contains(&tree.root()) {
            return Refresh::Root;
        }
    }
    if folders.is_empty() {
        Refresh::None
    } else {
        Refresh::Folders(folders.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScanOptions, scan};
    use std::fs;
    use std::io::Write;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    fn write_bytes(path: &Path, length: usize) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&vec![0u8; length]).unwrap();
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
    fn refresh_targets_picks_the_parent_of_a_change() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("a/b")).unwrap();
        fs::create_dir(root.path().join("c")).unwrap();
        write_bytes(&root.path().join("a/d.bin"), 100);
        write_bytes(&root.path().join("root.bin"), 100);
        let tree = scan(ScanOptions::new(root.path())).unwrap();
        let a = find(&tree, "a");
        let b = find(&tree, "a/b");
        let c = find(&tree, "c");

        assert_eq!(
            refresh_targets(&tree, &[root.path().join("a/new.bin")]),
            Refresh::Folders(vec![a])
        );
        assert_eq!(
            refresh_targets(&tree, &[root.path().join("a/d.bin")]),
            Refresh::Folders(vec![a])
        );
        assert_eq!(
            refresh_targets(&tree, &[root.path().join("a/b/x.bin")]),
            Refresh::Folders(vec![b])
        );
        assert_eq!(
            refresh_targets(&tree, &[root.path().join("a/b"), root.path().join("a")]),
            Refresh::Folders(vec![a]),
            "a child is dropped when its parent also changed"
        );
        assert_eq!(
            refresh_targets(&tree, &[root.path().join("a"), root.path().join("c")]),
            Refresh::Folders(vec![a, c])
        );
        assert_eq!(
            refresh_targets(&tree, &[root.path().join("root.bin")]),
            Refresh::Root
        );
        assert_eq!(
            refresh_targets(&tree, &[root.path().to_path_buf()]),
            Refresh::Root
        );
        assert_eq!(
            refresh_targets(&tree, &[PathBuf::from("/tmp/elsewhere")]),
            Refresh::None
        );
    }

    #[test]
    fn watch_reports_a_new_file_in_the_folder() {
        let root = tempfile::tempdir().unwrap();
        write_bytes(&root.path().join("a.bin"), 10);
        let (_watch, rx) = match Watch::start(root.path(), u64::MAX) {
            Ok(pair) => pair,
            Err(_) => return, // no fseventsd in restricted environments
        };
        thread::sleep(Duration::from_millis(400));
        write_bytes(&root.path().join("b.bin"), 10);

        let root_real = root.path().canonicalize().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut saw_root = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(batch) => {
                    if batch.paths.iter().any(|path| {
                        path.canonicalize()
                            .map(|real| real == root_real || real.starts_with(&root_real))
                            .unwrap_or(false)
                            || path == root.path()
                    }) {
                        saw_root = true;
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_root, "FSEvents should report the watched folder");
    }
}
