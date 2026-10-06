//! Which apps and command-line tools are running, so their caches are left alone.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_int, c_void};
use std::path::{Path, PathBuf};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSBundle, NSString};

#[derive(Debug, Clone, Default)]
pub struct RunningApps {
    /// Lowercased bundle IDs of running apps.
    bundle_ids: HashSet<String>,
    /// A live process whose path could not be read. That is not proof that nothing is running.
    unknown_processes: bool,
    /// Lowercased app names, from `Name.app`.
    app_names: HashSet<String>,
    /// Lowercased executable and script names without extension, from the process path and
    /// its first arguments, so `node …/npm` counts as `npm`.
    commands: HashSet<String>,
}

impl RunningApps {
    pub fn current() -> Self {
        let mut running = Self::default();
        let mut bundles: HashMap<PathBuf, Option<String>> = HashMap::new();
        for pid in all_pids() {
            match process_path(pid) {
                Ok(path) => running.add_path(&path, &mut bundles),
                Err(ProcessPathError::Gone) => {}
                Err(ProcessPathError::Unreadable) => running.unknown_processes = true,
            }
            for argument in arguments(pid).into_iter().take(2) {
                if let Some(name) = command_name(Path::new(&argument)) {
                    running.commands.insert(name);
                }
            }
        }
        running
    }

    /// For tests and callers that already know what is running.
    pub fn from_parts(
        bundle_ids: impl IntoIterator<Item = String>,
        app_names: impl IntoIterator<Item = String>,
        commands: impl IntoIterator<Item = String>,
    ) -> Self {
        let lower = |values: &mut dyn Iterator<Item = String>| -> HashSet<String> {
            values.map(|value| value.to_lowercase()).collect()
        };
        Self {
            bundle_ids: lower(&mut bundle_ids.into_iter()),
            unknown_processes: false,
            app_names: lower(&mut app_names.into_iter()),
            commands: lower(&mut commands.into_iter()),
        }
    }

    /// Marks the snapshot as missing at least one process path. Leftovers stay hidden, because
    /// an unreadable path is not proof that its app is absent.
    pub fn with_unreadable_process(mut self) -> Self {
        self.unknown_processes = true;
        self
    }

    pub fn has_unreadable_process(&self) -> bool {
        self.unknown_processes
    }

    pub fn bundle_ids(&self) -> impl Iterator<Item = &str> {
        self.bundle_ids.iter().map(String::as_str)
    }

    pub fn has_bundle(&self, id: &str) -> bool {
        self.bundle_ids.contains(&id.to_lowercase())
    }

    fn add_path(&mut self, path: &Path, bundles: &mut HashMap<PathBuf, Option<String>>) {
        if let Some(name) = command_name(path) {
            self.commands.insert(name);
        }
        let mut app = PathBuf::new();
        for component in path.components() {
            app.push(component);
            let is_app = Path::new(component.as_os_str())
                .extension()
                .is_some_and(|extension| extension == "app");
            if !is_app {
                continue;
            }
            if let Some(stem) = app.file_stem() {
                self.app_names.insert(stem.to_string_lossy().to_lowercase());
            }
            let bundle_id = bundles
                .entry(app.clone())
                .or_insert_with(|| bundle_identifier(&app));
            if let Some(id) = bundle_id {
                self.bundle_ids.insert(id.to_lowercase());
            }
        }
    }

    pub fn is_command_running(&self, names: &[&str]) -> bool {
        names.iter().any(|name| self.commands.contains(*name))
    }

    pub fn is_app_running(&self, name: &str) -> bool {
        self.app_names.contains(&name.to_lowercase())
    }

    /// Whether a folder in `~/Library/Caches` probably belongs to a running app. Caches are
    /// named after the bundle ID, or after the app or its vendor.
    pub fn owns_cache(&self, cache: &str) -> bool {
        let cache = cache.to_lowercase();
        if self.bundle_ids.contains(&cache) || self.app_names.contains(&cache) {
            return true;
        }
        if self
            .bundle_ids
            .iter()
            .any(|id| id.starts_with(&format!("{cache}.")) || cache.starts_with(&format!("{id}.")))
        {
            return true;
        }
        cache.len() >= 4
            && self
                .app_names
                .iter()
                .any(|name| name.split_whitespace().any(|word| word == cache))
    }
}

fn command_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    (!stem.is_empty()).then_some(stem)
}

fn all_pids() -> Vec<c_int> {
    // SAFETY: a null buffer asks for the number of processes.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as c_int; count as usize + 64];
    let bytes = (pids.len() * size_of::<c_int>()) as c_int;
    // SAFETY: `pids` has room for `bytes` bytes.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
    pids.truncate(count.max(0) as usize);
    pids.retain(|&pid| pid > 0);
    pids
}

enum ProcessPathError {
    /// The process exited between the list and the read.
    Gone,
    /// The path could not be read. This is not evidence that the process is not an app.
    Unreadable,
}

fn process_path(pid: c_int) -> Result<PathBuf, ProcessPathError> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buffer` has room for the size passed.
    let length =
        unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Err(ProcessPathError::Gone)
        } else {
            Err(ProcessPathError::Unreadable)
        };
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(String::from_utf8_lossy(&buffer).into_owned()))
}

/// The process's arguments (`KERN_PROCARGS2`). Only the user's own processes are readable.
fn arguments(pid: c_int) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: usize = 0;
    // SAFETY: a null buffer asks for the size.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size < 4 {
        return Vec::new();
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: `buffer` has room for `size` bytes.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size < 4 {
        return Vec::new();
    }
    buffer.truncate(size);
    let argc = i32::from_ne_bytes(buffer[..4].try_into().expect("4 bytes")).max(0) as usize;
    // Layout: argc, the executable path, NUL padding, then argc NUL-terminated arguments.
    let mut rest = &buffer[4..];
    let Some(end) = rest.iter().position(|&byte| byte == 0) else {
        return Vec::new();
    };
    rest = &rest[end..];
    let start = rest
        .iter()
        .position(|&byte| byte != 0)
        .unwrap_or(rest.len());
    rest = &rest[start..];
    let mut arguments = Vec::with_capacity(argc);
    while arguments.len() < argc && !rest.is_empty() {
        let Ok(argument) = CStr::from_bytes_until_nul(rest) else {
            break;
        };
        rest = &rest[argument.to_bytes().len() + 1..];
        arguments.push(argument.to_string_lossy().into_owned());
    }
    arguments
}

pub(crate) fn bundle_identifier(app: &Path) -> Option<String> {
    autoreleasepool(|_| {
        let bundle = NSBundle::bundleWithPath(&NSString::from_str(app.to_str()?))?;
        Some(bundle.bundleIdentifier()?.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_this_test_process() {
        let running = RunningApps::current();
        let exe = std::env::current_exe().unwrap();
        let name = command_name(&exe).unwrap();
        assert!(running.is_command_running(&[name.as_str()]));
    }

    #[test]
    fn matches_caches_to_apps() {
        let running = RunningApps::from_parts(
            [
                "com.spotify.client".to_string(),
                "com.google.Chrome".to_string(),
            ],
            ["Spotify".to_string(), "Google Chrome".to_string()],
            ["npm".to_string()],
        );
        assert!(running.owns_cache("com.spotify.client"));
        assert!(running.owns_cache("Spotify"));
        assert!(running.owns_cache("Google"));
        assert!(running.owns_cache("com.google"));
        assert!(!running.owns_cache("com.tinyspeck.slackmacgap"));
        assert!(running.is_command_running(&["npm"]));
        assert!(!running.is_command_running(&["yarn"]));
    }
}
