//! Checks GitHub for a newer release and, when one exists, replaces the installed app.
//!
//! The request is anonymous. A private repository answers 404, and no token is built into the app.
//! The download has to be this project's own `Mac-Storage-Cleaner-<version>.dmg`. Nothing else is
//! fetched or opened.

use std::cmp::Ordering;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/07rjain/mac-storage-cleaner/releases/latest";
const RELEASE_PREFIX: &str = "https://github.com/07rjain/mac-storage-cleaner/";
const APP_BUNDLE_NAME: &str = "Mac Storage Cleaner.app";
const BUNDLE_ID: &str = "io.github.07rjain.mac-storage-cleaner";
const EXECUTABLE_NAME: &str = "mac-storage-cleaner";
const APPLICATIONS: &str = "/Applications/Mac Storage Cleaner.app";
const MAX_DMG_BYTES: u64 = 250 * 1024 * 1024;

/// Waits for this process to exit, swaps in the staged app, and reopens it.
/// Arguments are `$1` pid, `$2` staged app, `$3` destination, `$4` work dir, `$5` opener.
const REPLACER: &str = r#"
pid="$1"
staged="$2"
dest="$3"
workdir="$4"
opener="${5:-/usr/bin/open}"

case "$dest" in
  */"Mac Storage Cleaner.app") ;;
  *) exit 2 ;;
esac

if [ ! -d "$staged/Contents/MacOS" ]; then
  exit 3
fi

i=0
while /bin/kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 240 ]; then
    exit 4
  fi
  /bin/sleep 0.25
done

next="$dest.installing"
/bin/rm -rf "$next"
if ! /usr/bin/ditto "$staged" "$next"; then
  /bin/rm -rf "$next"
  if [ -d "$dest" ]; then
    "$opener" "$dest"
  fi
  /bin/rm -rf "$workdir"
  exit 1
fi
/usr/bin/xattr -cr "$next"
if ! /bin/rm -rf "$dest"; then
  /bin/rm -rf "$next"
  if [ -d "$dest" ]; then
    "$opener" "$dest"
  fi
  /bin/rm -rf "$workdir"
  exit 1
fi
if ! /bin/mv "$next" "$dest"; then
  /bin/rm -rf "$next"
  /bin/rm -rf "$workdir"
  exit 1
fi
"$opener" "$dest"
/bin/rm -rf "$workdir"
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub dmg: Option<String>,
}

/// `true` when `latest` is a higher `major.minor.patch` than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    parts(latest).cmp(&parts(current)) == Ordering::Greater
}

fn parts(version: &str) -> [u64; 3] {
    let version = version.trim().trim_start_matches('v');
    let mut numbers = [0; 3];
    for (slot, part) in version.split('.').take(3).enumerate() {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        numbers[slot] = digits.parse().unwrap_or(0);
    }
    numbers
}

/// Only this project's own GitHub release URLs. An unexpected address is not opened.
pub fn is_release_url(url: &str) -> bool {
    url.starts_with(RELEASE_PREFIX)
        && url
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control() && c != ' ')
}

pub fn fetch_latest() -> Result<Release, String> {
    let output = Command::new("/usr/bin/curl")
        .args([
            "-sS",
            "-m",
            "20",
            "-w",
            "\n%{http_code}",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: mac-storage-cleaner",
            LATEST,
        ])
        .output()
        .map_err(|error| format!("Couldn't start the update check: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((text.as_ref(), ""));
    match code.trim() {
        "200" => parse_release(body).ok_or_else(|| "The release list was unreadable.".into()),
        "404" => Err(
            "Releases aren't visible. The GitHub repository has to be public for the app to see them."
                .into(),
        ),
        "000" => {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            if detail.is_empty() {
                Err("Couldn't reach GitHub.".into())
            } else {
                Err(format!("Couldn't reach GitHub: {detail}"))
            }
        }
        other => Err(format!("GitHub returned status {other}.")),
    }
}

pub fn parse_release(body: &str) -> Option<Release> {
    let payload: Payload = serde_json::from_str(body).ok()?;
    let version = payload.tag_name.trim().trim_start_matches('v').to_string();
    if version.is_empty() || !is_release_url(&payload.html_url) {
        return None;
    }
    let expected = format!("Mac-Storage-Cleaner-{version}.dmg");
    let dmg = payload.assets.into_iter().find_map(|asset| {
        (asset.name == expected && is_dmg_url(&asset.browser_download_url, &version))
            .then_some(asset.browser_download_url)
    });
    Some(Release {
        version,
        page: payload.html_url,
        dmg,
    })
}

/// `true` when `url` is this project's disk image for `version`.
pub fn is_dmg_url(url: &str, version: &str) -> bool {
    let name = format!("Mac-Storage-Cleaner-{version}.dmg");
    is_release_url(url) && url.ends_with(&format!("/{name}"))
}

/// The `.app` bundle that contains `executable`, when this process was launched from one.
pub fn app_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    if bundle.file_name()? != APP_BUNDLE_NAME {
        return None;
    }
    Some(bundle.to_path_buf())
}

/// Where an update should be written. A disk image or a translocated copy goes to Applications.
/// A development binary has nowhere safe to replace.
pub fn install_target(bundle: Option<&Path>) -> Result<PathBuf, String> {
    let Some(bundle) = bundle else {
        return Err(
            "This copy was started from a development build, so it can't replace itself.".into(),
        );
    };
    let text = bundle.to_str().unwrap_or("");
    if text.contains("AppTranslocation") || text.starts_with("/Volumes/") {
        return Ok(PathBuf::from(APPLICATIONS));
    }
    if bundle.file_name().and_then(|name| name.to_str()) != Some(APP_BUNDLE_NAME) {
        return Err("Couldn't find the installed app to replace.".into());
    }
    Ok(bundle.to_path_buf())
}

/// Downloads the release and starts the swap. The caller then quits so the swap can finish.
pub fn install_update(url: &str, version: &str) -> Result<(), String> {
    if !is_dmg_url(url, version) {
        return Err("The update address isn't a release of this app.".into());
    }
    let executable =
        std::env::current_exe().map_err(|_| "Couldn't find this app to replace.".to_string())?;
    let destination = install_target(app_bundle(&executable).as_deref())?;
    let destination = settled_destination(&destination)?;
    let work = work_dir()?;
    let outcome = stage_update(url, &work);
    let staged = match outcome {
        Ok(staged) => staged,
        Err(error) => {
            let _ = fs::remove_dir_all(&work);
            return Err(error);
        }
    };
    if destination.starts_with(&work) {
        let _ = fs::remove_dir_all(&work);
        return Err("Couldn't find a place to install the update.".into());
    }
    match spawn_replacer(&staged, &destination, &work, "/usr/bin/open") {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_dir_all(&work);
            Err(error)
        }
    }
}

fn stage_update(url: &str, work: &Path) -> Result<PathBuf, String> {
    let dmg = work.join("update.dmg");
    download(url, &dmg)?;
    if !is_disk_image(&dmg) {
        return Err("The download isn't a disk image.".into());
    }
    let mount_point = work.join("mount");
    fs::create_dir(&mount_point).map_err(|_| "Couldn't open the disk image.".to_string())?;
    let mount = Mount::attach(&dmg, &mount_point)?;
    let source = bundled_app(mount.path())?;
    let staged = work.join(APP_BUNDLE_NAME);
    let copied = Command::new("/usr/bin/ditto")
        .arg(&source)
        .arg(&staged)
        .status()
        .map_err(|_| "Couldn't copy the new app.".to_string())?;
    drop(mount);
    if !copied.success() {
        return Err("Couldn't copy the new app.".into());
    }
    let _ = fs::remove_file(&dmg);
    bundled_app(work)?;
    Ok(staged)
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("/usr/bin/curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "5",
            "--max-time",
            "300",
            "--retry",
            "2",
            "-o",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|_| "Couldn't start the download.".to_string())?;
    if !status.success() {
        return Err("Couldn't download the update.".into());
    }
    Ok(())
}

fn is_disk_image(path: &Path) -> bool {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    let len = meta.len();
    if !(512..=MAX_DMG_BYTES).contains(&len) {
        return false;
    }
    if file.seek(SeekFrom::End(-512)).is_err() {
        return false;
    }
    let mut magic = [0; 4];
    file.read_exact(&mut magic).is_ok() && magic == *b"koly"
}

fn bundled_app(root: &Path) -> Result<PathBuf, String> {
    let app = root.join(APP_BUNDLE_NAME);
    let meta = fs::symlink_metadata(&app)
        .map_err(|_| "The disk image doesn't contain Mac Storage Cleaner.".to_string())?;
    if !meta.is_dir() {
        return Err("The disk image doesn't contain Mac Storage Cleaner.".into());
    }
    let plist = fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|_| "The disk image doesn't contain Mac Storage Cleaner.".to_string())?;
    if !plist.contains(BUNDLE_ID) {
        return Err("The disk image doesn't contain Mac Storage Cleaner.".into());
    }
    let executable = app.join("Contents/MacOS").join(EXECUTABLE_NAME);
    let executable = fs::symlink_metadata(&executable)
        .map_err(|_| "The disk image doesn't contain Mac Storage Cleaner.".to_string())?;
    if !executable.is_file() {
        return Err("The disk image doesn't contain Mac Storage Cleaner.".into());
    }
    Ok(app)
}

fn settled_destination(path: &Path) -> Result<PathBuf, String> {
    if path.file_name().and_then(|name| name.to_str()) != Some(APP_BUNDLE_NAME) {
        return Err("Couldn't find the installed app to replace.".into());
    }
    let parent = path
        .parent()
        .filter(|parent| parent.exists())
        .ok_or_else(|| "Couldn't find a place to install the update.".to_string())?;
    let parent = parent
        .canonicalize()
        .map_err(|_| "Couldn't find a place to install the update.".to_string())?;
    Ok(parent.join(APP_BUNDLE_NAME))
}

fn work_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mac-storage-cleaner-update-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir(&dir).map_err(|_| "Couldn't start the update.".to_string())?;
    Ok(dir)
}

fn spawn_replacer(staged: &Path, dest: &Path, work: &Path, opener: &str) -> Result<(), String> {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(REPLACER)
        .arg("replace")
        .arg(std::process::id().to_string())
        .arg(staged)
        .arg(dest)
        .arg(work)
        .arg(opener)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Couldn't switch to the new version.".to_string())
}

struct Mount {
    path: PathBuf,
}

impl Mount {
    fn attach(dmg: &Path, mount_point: &Path) -> Result<Self, String> {
        let status = Command::new("/usr/bin/hdiutil")
            .arg("attach")
            .arg("-nobrowse")
            .arg("-readonly")
            .arg("-mountpoint")
            .arg(mount_point)
            .arg(dmg)
            .status()
            .map_err(|_| "Couldn't open the disk image.".to_string())?;
        if !status.success() {
            return Err("Couldn't open the disk image.".into());
        }
        Ok(Self {
            path: mount_point.to_path_buf(),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg("-quiet")
            .arg(&self.path)
            .status();
    }
}

#[derive(Deserialize)]
struct Payload {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_major_minor_and_patch() {
        assert!(is_newer("0.2.2", "0.2.1"));
        assert!(is_newer("v0.3.0", "0.2.9"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(is_newer("1.0", "0.9.9"));
    }

    #[test]
    fn parses_a_release_and_keeps_only_its_dmg() {
        let body = r#"{
            "tag_name": "v0.2.2",
            "html_url": "https://github.com/07rjain/mac-storage-cleaner/releases/tag/v0.2.2",
            "assets": [
                {
                    "name": "notes.txt",
                    "browser_download_url": "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.2/notes.txt"
                },
                {
                    "name": "Mac-Storage-Cleaner-0.2.2.dmg",
                    "browser_download_url": "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.2/Mac-Storage-Cleaner-0.2.2.dmg"
                }
            ]
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.version, "0.2.2");
        assert_eq!(
            release.dmg.as_deref(),
            Some(
                "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.2/Mac-Storage-Cleaner-0.2.2.dmg"
            )
        );
    }

    #[test]
    fn rejects_a_release_that_points_somewhere_else() {
        let body = r#"{
            "tag_name": "v9.9.9",
            "html_url": "https://example.com/evil",
            "assets": [{
                "name": "Mac-Storage-Cleaner-9.9.9.dmg",
                "browser_download_url": "https://example.com/Mac-Storage-Cleaner-9.9.9.dmg"
            }]
        }"#;
        assert!(parse_release(body).is_none());
        assert!(!is_release_url(
            "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.1/a.dmg\nhttps://evil"
        ));
    }

    #[test]
    fn ignores_a_disk_image_with_the_wrong_name() {
        let body = r#"{
            "tag_name": "v0.2.5",
            "html_url": "https://github.com/07rjain/mac-storage-cleaner/releases/tag/v0.2.5",
            "assets": [{
                "name": "Other.dmg",
                "browser_download_url": "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.5/Other.dmg"
            }]
        }"#;
        let release = parse_release(body).unwrap();
        assert!(release.dmg.is_none());
        assert!(!is_dmg_url(
            "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.5/Other.dmg",
            "0.2.5"
        ));
    }

    #[test]
    fn install_target_replaces_the_running_bundle_or_applications() {
        let installed = Path::new("/Users/example/Applications/Mac Storage Cleaner.app");
        assert_eq!(install_target(Some(installed)).unwrap(), installed);
        let translocated =
            Path::new("/private/var/folders/xx/AppTranslocation/UUID/d/Mac Storage Cleaner.app");
        assert_eq!(
            install_target(Some(translocated)).unwrap(),
            PathBuf::from(APPLICATIONS)
        );
        assert_eq!(
            install_target(Some(Path::new(
                "/Volumes/Mac Storage Cleaner/Mac Storage Cleaner.app"
            )))
            .unwrap(),
            PathBuf::from(APPLICATIONS)
        );
        assert!(install_target(None).is_err());
        assert!(app_bundle(Path::new("/tmp/mac-storage-cleaner")).is_none());
        assert_eq!(
            app_bundle(Path::new(
                "/Applications/Mac Storage Cleaner.app/Contents/MacOS/mac-storage-cleaner"
            ))
            .unwrap(),
            PathBuf::from("/Applications/Mac Storage Cleaner.app")
        );
    }

    #[test]
    fn replacer_swaps_the_app_and_clears_quarantine() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        let staged = work.join("Mac Storage Cleaner.app");
        fs::create_dir_all(staged.join("Contents/MacOS")).unwrap();
        fs::write(staged.join("Contents/MacOS/mac-storage-cleaner"), "new").unwrap();
        let _ = Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", "0083;00000000;;"])
            .arg(staged.join("Contents/MacOS/mac-storage-cleaner"))
            .status();

        let dest_parent = root.path().join("Applications");
        fs::create_dir_all(&dest_parent).unwrap();
        let dest = dest_parent.join("Mac Storage Cleaner.app");
        fs::create_dir_all(dest.join("Contents")).unwrap();
        fs::write(dest.join("Contents/old"), "old").unwrap();

        let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();

        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(REPLACER)
            .arg("replace")
            .arg(pid.to_string())
            .arg(&staged)
            .arg(&dest)
            .arg(&work)
            .arg("/usr/bin/true")
            .status()
            .unwrap();
        assert!(status.success(), "replacer status {status}");
        assert_eq!(
            fs::read(dest.join("Contents/MacOS/mac-storage-cleaner")).unwrap(),
            b"new"
        );
        assert!(!dest.join("Contents/old").exists());
        let attrs = Command::new("/usr/bin/xattr")
            .arg("-l")
            .arg(dest.join("Contents/MacOS/mac-storage-cleaner"))
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&attrs.stdout);
        assert!(
            !listed.contains("com.apple.quarantine"),
            "quarantine still set: {listed}"
        );
        assert!(!work.exists());
    }

    #[test]
    fn replacer_refuses_a_destination_that_is_not_the_app() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("notes.txt");
        fs::write(&dest, "keep").unwrap();
        let staged = root.path().join("staged");
        fs::create_dir_all(staged.join("Contents/MacOS")).unwrap();
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(REPLACER)
            .arg("replace")
            .arg("1")
            .arg(&staged)
            .arg(&dest)
            .arg(root.path())
            .arg("/usr/bin/true")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2));
        assert_eq!(fs::read(&dest).unwrap(), b"keep");
    }
}
