//! Accuracy check against a real, freshly created APFS volume.
//!
//! Run with `cargo test -p scanner --test apfs_image -- --ignored --nocapture`.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use scanner::ScanOptions;

/// A fresh APFS volume holds a few megabytes of metadata that no directory walk can see.
const METADATA_ALLOWANCE: u64 = 16 * 1024 * 1024;

struct MountedImage<'a> {
    mount_point: &'a Path,
}

impl Drop for MountedImage<'_> {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .args(["detach", "-force"])
            .arg(self.mount_point)
            .output();
    }
}

fn run(command: &mut Command) {
    let output = command.output().expect("command runs");
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_incompressible(path: &Path, length: usize, seed: u32) {
    let mut state = seed | 1;
    let bytes: Vec<u8> = (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
#[ignore = "creates and mounts a disk image with hdiutil"]
fn scan_matches_volume_usage_on_a_fresh_apfs_volume() {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("scanner-test.dmg");
    let mount_point = work.path().join("mnt");
    fs::create_dir(&mount_point).unwrap();

    run(Command::new("/usr/bin/hdiutil")
        .args([
            "create",
            "-size",
            "256m",
            "-fs",
            "APFS",
            "-volname",
            "ScannerTest",
        ])
        .arg(&image));
    run(Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-mountpoint"])
        .arg(&mount_point)
        .arg(&image));
    let _mounted = MountedImage {
        mount_point: &mount_point,
    };

    fs::create_dir_all(mount_point.join("photos/2026")).unwrap();
    for index in 0..20 {
        write_incompressible(
            &mount_point.join(format!("photos/2026/img-{index}.raw")),
            1_000_000,
            index,
        );
    }
    write_incompressible(&mount_point.join("video.mov"), 30_000_000, 99);
    fs::hard_link(
        mount_point.join("video.mov"),
        mount_point.join("video-link.mov"),
    )
    .unwrap();
    fs::File::create(mount_point.join("sparse.img"))
        .unwrap()
        .set_len(100_000_000)
        .unwrap();
    // SAFETY: `sync` has no preconditions.
    unsafe { libc::sync() };

    let tree = scanner::scan(ScanOptions::new(&mount_point)).unwrap();
    let scanned = tree.allocated(tree.root());
    let used = volumes::volume_at(&mount_point).unwrap().used;

    println!(
        "scanned {scanned} bytes, volume reports {used} bytes used, difference {} bytes",
        used as i64 - scanned as i64
    );
    assert!(scanned >= 50_000_000, "expected at least the 50 MB written");
    assert!(scanned <= used, "scan counted more than the volume holds");
    assert!(
        used - scanned <= METADATA_ALLOWANCE,
        "{} bytes on the volume were not found by the scan",
        used - scanned
    );
}
