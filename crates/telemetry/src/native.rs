//! Native crash capture: a signal handler records instruction addresses and loaded-image
//! identities, then the next launch sends them to Sentry.
//!
//! The dump never includes file paths, user names, or process memory. Image names are the
//! library file name only (`libsystem_c.dylib`, `Mac Storage Cleaner`).

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read};
use std::mem;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use sentry::protocol::{
    Addr, AppleDebugImage, DebugImage, DebugMeta, Event, Exception, Frame, Level, Mechanism,
    MechanismMeta, PosixSignal, Stacktrace,
};
use sentry::types::Uuid;

const MAGIC: [u8; 4] = *b"MSCC";
const VERSION: u32 = 1;
const MAX_FRAMES: usize = 64;
const MAX_IMAGES: usize = 256;
const NAME_LEN: usize = 48;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UUID: u32 = 0x1b;

static PANICKING: AtomicBool = AtomicBool::new(false);
static CRASH_FD: AtomicI32 = AtomicI32::new(-1);
static IMAGE_COUNT: AtomicU32 = AtomicU32::new(0);
static mut IMAGES: [Image; MAX_IMAGES] = [Image::empty(); MAX_IMAGES];

#[repr(C)]
#[derive(Clone, Copy)]
struct Image {
    addr: u64,
    size: u64,
    uuid: [u8; 16],
    name: [u8; NAME_LEN],
}

impl Image {
    const fn empty() -> Self {
        Self {
            addr: 0,
            size: 0,
            uuid: [0; 16],
            name: [0; NAME_LEN],
        }
    }
}

#[repr(C)]
struct Header {
    magic: [u8; 4],
    version: u32,
    signal: i32,
    code: i32,
    frame_count: u32,
    image_count: u32,
    frames: [u64; MAX_FRAMES],
}

#[repr(C)]
struct MachHeader64 {
    magic: u32,
    cputype: i32,
    cpusubtype: i32,
    filetype: u32,
    ncmds: u32,
    sizeofcmds: u32,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct LoadCommand {
    cmd: u32,
    cmdsize: u32,
}

#[repr(C)]
struct UuidCommand {
    cmd: u32,
    cmdsize: u32,
    uuid: [u8; 16],
}

#[repr(C)]
struct SegmentCommand64 {
    cmd: u32,
    cmdsize: u32,
    segname: [u8; 16],
    vmaddr: u64,
    vmsize: u64,
}

unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_header(image_index: u32) -> *const MachHeader64;
    fn _dyld_get_image_name(image_index: u32) -> *const i8;
}

/// Parsed dump, for tests and for turning into a Sentry event.
#[derive(Debug, Clone)]
pub struct Dump {
    pub signal: i32,
    pub code: i32,
    pub frames: Vec<u64>,
    pub images: Vec<DumpImage>,
}

#[derive(Debug, Clone)]
pub struct DumpImage {
    pub addr: u64,
    pub size: u64,
    pub uuid: [u8; 16],
    pub name: String,
}

/// After Sentry is initialized: wrap the panic hook, install handlers, return a pending crash.
pub fn install(path: &Path) -> Option<Event<'static>> {
    let pending = load(path);
    wrap_panic_hook();
    snapshot_images();
    if open_crash_file(path).is_err() {
        return pending.map(Dump::into_event);
    }
    install_handlers();
    pending.map(Dump::into_event)
}

fn wrap_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICKING.store(true, Ordering::SeqCst);
        previous(info);
    }));
}

fn load(path: &Path) -> Option<Dump> {
    let mut file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    parse(&bytes)
}

pub fn parse(bytes: &[u8]) -> Option<Dump> {
    let header_size = mem::size_of::<Header>();
    if bytes.len() < header_size {
        return None;
    }
    let header = unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<Header>()) };
    if header.magic != MAGIC || header.version != VERSION {
        return None;
    }
    let frame_count = header.frame_count.min(MAX_FRAMES as u32) as usize;
    let image_count = header.image_count.min(MAX_IMAGES as u32) as usize;
    let images_start = header_size;
    let images_end = images_start.checked_add(image_count * mem::size_of::<Image>())?;
    if bytes.len() < images_end {
        return None;
    }
    let mut images = Vec::with_capacity(image_count);
    let mut offset = images_start;
    for _ in 0..image_count {
        let image = unsafe { ptr::read_unaligned(bytes[offset..].as_ptr().cast::<Image>()) };
        offset += mem::size_of::<Image>();
        images.push(DumpImage {
            addr: image.addr,
            size: image.size,
            uuid: image.uuid,
            name: image_name(&image.name),
        });
    }
    Some(Dump {
        signal: header.signal,
        code: header.code,
        frames: header.frames[..frame_count].to_vec(),
        images,
    })
}

impl Dump {
    pub fn into_event(self) -> Event<'static> {
        let mut event = Event::new();
        event.level = Level::Fatal;
        let frames = self
            .frames
            .iter()
            .rev()
            .map(|&addr| Frame {
                instruction_addr: Some(Addr(addr)),
                ..Default::default()
            })
            .collect();
        event.exception.values.push(Exception {
            ty: signal_name(self.signal).into(),
            value: None,
            stacktrace: Some(Stacktrace {
                frames,
                ..Default::default()
            }),
            mechanism: Some(Mechanism {
                ty: "signal".into(),
                handled: Some(false),
                meta: MechanismMeta {
                    signal: Some(PosixSignal {
                        number: self.signal,
                        code: Some(self.code),
                        name: Some(signal_name(self.signal).into()),
                        code_name: None,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        });
        event.debug_meta = Cow::Owned(DebugMeta {
            sdk_info: None,
            images: self
                .images
                .into_iter()
                .filter(|image| image.addr != 0 && image.uuid != [0; 16])
                .map(|image| {
                    DebugImage::Apple(AppleDebugImage {
                        name: image.name,
                        arch: Some("arm64".into()),
                        cpu_type: None,
                        cpu_subtype: None,
                        image_addr: Addr(image.addr),
                        image_size: image.size,
                        image_vmaddr: Addr(0),
                        uuid: Uuid::from_bytes(image.uuid),
                    })
                })
                .collect(),
        });
        event
    }
}

fn image_name(bytes: &[u8; NAME_LEN]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGBUS => "SIGBUS",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGFPE => "SIGFPE",
        _ => "signal",
    }
}

fn open_crash_file(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    CRASH_FD.store(fd, Ordering::SeqCst);
    mem::forget(unsafe { OwnedFd::from_raw_fd(fd) });
    Ok(())
}

fn snapshot_images() {
    let count = unsafe { _dyld_image_count() }.min(MAX_IMAGES as u32);
    let mut stored = 0u32;
    for index in 0..count {
        let Some(image) = read_image(index) else {
            continue;
        };
        unsafe {
            ptr::write(addr_of_mut!(IMAGES[stored as usize]), image);
        }
        stored += 1;
        if stored == MAX_IMAGES as u32 {
            break;
        }
    }
    IMAGE_COUNT.store(stored, Ordering::Release);
}

fn read_image(index: u32) -> Option<Image> {
    let header = unsafe { _dyld_get_image_header(index) };
    if header.is_null() {
        return None;
    }
    let magic = unsafe { (*header).magic };
    if magic != MH_MAGIC_64 {
        return None;
    }
    let (uuid, size) = uuid_and_size(header)?;
    if uuid == [0; 16] {
        return None;
    }
    let name_ptr = unsafe { _dyld_get_image_name(index) };
    Some(Image {
        addr: header as u64,
        size,
        uuid,
        name: basename(name_ptr),
    })
}

fn basename(ptr: *const i8) -> [u8; NAME_LEN] {
    let mut name = [0u8; NAME_LEN];
    if ptr.is_null() {
        return name;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_bytes();
    let file = bytes.rsplit(|b| *b == b'/').next().unwrap_or(bytes);
    let n = file.len().min(NAME_LEN - 1);
    name[..n].copy_from_slice(&file[..n]);
    name
}

fn uuid_and_size(header: *const MachHeader64) -> Option<([u8; 16], u64)> {
    unsafe {
        let ncmds = (*header).ncmds;
        let mut cursor = header.add(1) as *const u8;
        let end = cursor.add((*header).sizeofcmds as usize);
        let mut uuid = [0u8; 16];
        let mut min_addr = u64::MAX;
        let mut max_addr = 0u64;
        for _ in 0..ncmds {
            if cursor.add(mem::size_of::<LoadCommand>()) > end {
                break;
            }
            let cmd = ptr::read_unaligned(cursor.cast::<LoadCommand>());
            if cmd.cmdsize < mem::size_of::<LoadCommand>() as u32 {
                break;
            }
            match cmd.cmd {
                LC_UUID => {
                    let uuid_cmd = ptr::read_unaligned(cursor.cast::<UuidCommand>());
                    uuid = uuid_cmd.uuid;
                }
                LC_SEGMENT_64 => {
                    let seg = ptr::read_unaligned(cursor.cast::<SegmentCommand64>());
                    if seg.vmsize > 0 {
                        min_addr = min_addr.min(seg.vmaddr);
                        max_addr = max_addr.max(seg.vmaddr.saturating_add(seg.vmsize));
                    }
                }
                _ => {}
            }
            cursor = cursor.add(cmd.cmdsize as usize);
        }
        let size = if min_addr == u64::MAX {
            0
        } else {
            max_addr.saturating_sub(min_addr)
        };
        Some((uuid, size))
    }
}

fn install_handlers() {
    let mut action: libc::sigaction = unsafe { mem::zeroed() };
    action.sa_sigaction = handler as *const () as usize;
    action.sa_flags = libc::SA_SIGINFO;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [
            libc::SIGSEGV,
            libc::SIGBUS,
            libc::SIGILL,
            libc::SIGABRT,
            libc::SIGFPE,
        ] {
            libc::sigaction(signal, &action, ptr::null_mut());
        }
    }
}

extern "C" fn handler(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    if signal == libc::SIGABRT && PANICKING.load(Ordering::SeqCst) {
        restore_and_reraise(signal);
        return;
    }
    let fd = CRASH_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        write_dump(fd, signal, info, context);
    }
    restore_and_reraise(signal);
}

fn write_dump(fd: i32, signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let code = if info.is_null() {
        0
    } else {
        unsafe { (*info).si_code }
    };
    let (pc, fp) = program_counter(context);
    let mut frames = [0u64; MAX_FRAMES];
    let frame_count = walk_frames(pc, fp, &mut frames);
    let image_count = IMAGE_COUNT.load(Ordering::Acquire).min(MAX_IMAGES as u32);
    let header = Header {
        magic: MAGIC,
        version: VERSION,
        signal,
        code,
        frame_count,
        image_count,
        frames,
    };
    signal_write(fd, addr_of!(header).cast(), mem::size_of::<Header>());
    if image_count > 0 {
        signal_write(
            fd,
            addr_of!(IMAGES).cast(),
            image_count as usize * mem::size_of::<Image>(),
        );
    }
    unsafe {
        libc::fsync(fd);
    }
}

fn program_counter(context: *mut libc::c_void) -> (u64, u64) {
    if context.is_null() {
        return (0, 0);
    }
    let uc = context as *mut libc::ucontext_t;
    let mc = unsafe { (*uc).uc_mcontext };
    if mc.is_null() {
        return (0, 0);
    }
    unsafe { ((*mc).__ss.__pc, (*mc).__ss.__fp) }
}

fn walk_frames(pc: u64, fp: u64, out: &mut [u64]) -> u32 {
    if out.is_empty() {
        return 0;
    }
    let mut n = 0usize;
    if pc != 0 {
        out[0] = pc;
        n = 1;
    }
    let mut fp = fp as *const u64;
    while n < out.len() {
        let addr = fp as usize;
        if addr < 0x1000 || addr & 0x7 != 0 {
            break;
        }
        let saved_fp = unsafe { fp.read() };
        let lr = unsafe { fp.add(1).read() };
        if lr != 0 {
            out[n] = lr;
            n += 1;
        }
        if saved_fp as usize <= addr {
            break;
        }
        fp = saved_fp as *const u64;
    }
    n as u32
}

fn signal_write(fd: i32, ptr: *const u8, len: usize) {
    let mut written = 0usize;
    while written < len {
        let n = unsafe { libc::write(fd, ptr.add(written).cast(), len - written) };
        if n <= 0 {
            return;
        }
        written += n as usize;
    }
}

fn restore_and_reraise(signal: i32) {
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Encode a dump as the handler would write it. Used by tests.
#[cfg(test)]
fn encode(dump: &Dump) -> Vec<u8> {
    let mut header = Header {
        magic: MAGIC,
        version: VERSION,
        signal: dump.signal,
        code: dump.code,
        frame_count: dump.frames.len().min(MAX_FRAMES) as u32,
        image_count: dump.images.len().min(MAX_IMAGES) as u32,
        frames: [0; MAX_FRAMES],
    };
    for (slot, frame) in header.frames.iter_mut().zip(&dump.frames) {
        *slot = *frame;
    }
    let mut bytes =
        Vec::with_capacity(mem::size_of::<Header>() + dump.images.len() * mem::size_of::<Image>());
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(addr_of!(header).cast(), mem::size_of::<Header>())
    });
    for image in dump.images.iter().take(MAX_IMAGES) {
        let mut rec = Image::empty();
        rec.addr = image.addr;
        rec.size = image.size;
        rec.uuid = image.uuid;
        let name = image.name.as_bytes();
        let n = name.len().min(NAME_LEN - 1);
        rec.name[..n].copy_from_slice(&name[..n]);
        bytes.extend_from_slice(unsafe {
            std::slice::from_raw_parts(addr_of!(rec).cast(), mem::size_of::<Image>())
        });
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrub;

    #[test]
    fn dump_round_trips_and_keeps_only_the_library_file_name() {
        let dump = Dump {
            signal: libc::SIGSEGV,
            code: 1,
            frames: vec![0x1000_0000, 0x1000_0100],
            images: vec![DumpImage {
                addr: 0x1000_0000,
                size: 0x4000,
                uuid: [0x11; 16],
                name: "Mac Storage Cleaner".into(),
            }],
        };
        let parsed = parse(&encode(&dump)).expect("dump parses");
        assert_eq!(parsed.signal, libc::SIGSEGV);
        assert_eq!(parsed.frames, dump.frames);
        assert_eq!(parsed.images[0].name, "Mac Storage Cleaner");
    }

    #[test]
    fn event_has_signal_frames_and_no_paths() {
        let dump = Dump {
            signal: libc::SIGBUS,
            code: 2,
            frames: vec![0xabc, 0xdef],
            images: vec![DumpImage {
                addr: 0x1000,
                size: 0x2000,
                uuid: [0x22; 16],
                name: "libsystem_c.dylib".into(),
            }],
        };
        let event = dump.into_event();
        let exception = &event.exception.values[0];
        assert_eq!(exception.ty, "SIGBUS");
        assert_eq!(exception.mechanism.as_ref().unwrap().ty, "signal");
        assert_eq!(exception.mechanism.as_ref().unwrap().handled, Some(false));
        let frames = &exception.stacktrace.as_ref().unwrap().frames;
        assert_eq!(frames.last().unwrap().instruction_addr, Some(Addr(0xabc)));
        let json = serde_json::to_string(&scrub::event(event).unwrap()).unwrap();
        assert!(!json.contains("/Users/"), "{json}");
        assert!(json.contains("libsystem_c.dylib"), "{json}");
        assert!(json.contains("instruction_addr"), "{json}");
    }

    #[test]
    fn basename_strips_directories() {
        let c = std::ffi::CString::new("/Users/bob/Library/App.app/Contents/MacOS/App").unwrap();
        let name = basename(c.as_ptr());
        assert_eq!(image_name(&name), "App");
    }

    #[test]
    fn loaded_images_have_basenames_and_uuids() {
        snapshot_images();
        let count = IMAGE_COUNT.load(Ordering::Acquire) as usize;
        assert!(count > 0, "dyld lists this process's images");
        let images = unsafe { std::slice::from_raw_parts(addr_of!(IMAGES).cast::<Image>(), count) };
        assert!(
            images
                .iter()
                .any(|image| !image_name(&image.name).is_empty())
        );
        for image in images {
            let name = image_name(&image.name);
            assert!(!name.contains('/'), "{name}");
            assert_ne!(image.uuid, [0; 16]);
            assert!(image.addr != 0);
        }
    }
}
