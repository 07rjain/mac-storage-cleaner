//! The M3 exit check: on a throwaway APFS volume with no snapshots, the free space actually
//! gained by a cleanup is within 1% of the basket's "Will free".
//!
//! Run with `cargo test -p cleanup --test will_free -- --ignored --nocapture`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use cleanup::{Basket, Category, Places, RunningApps};
use scanner::ScanOptions;

struct MountedImage {
    mount_point: PathBuf,
}

impl Drop for MountedImage {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .args(["detach", "-force"])
            .arg(&self.mount_point)
            .output();
    }
}

fn run(command: &mut Command) -> String {
    let output = command.output().expect("command runs");
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write_incompressible(path: &Path, length: usize, seed: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
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

fn available(mount_point: &Path) -> u64 {
    volumes::volume_at(mount_point).unwrap().available
}

/// APFS frees blocks shortly after the unlink returns; wait until the number stops moving.
fn settled_available(mount_point: &Path) -> u64 {
    // SAFETY: `sync` has no preconditions.
    unsafe { libc::sync() };
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = available(mount_point);
    let mut steady = 0;
    while Instant::now() < deadline && steady < 3 {
        std::thread::sleep(Duration::from_millis(300));
        let now = available(mount_point);
        steady = if now == last { steady + 1 } else { 0 };
        last = now;
    }
    last
}

#[test]
#[ignore = "creates and mounts a disk image with hdiutil"]
fn free_space_gained_matches_will_free() {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("will-free.dmg");
    let volume = format!("MSCWillFree{}", std::process::id());
    run(Command::new("/usr/bin/hdiutil")
        .args([
            "create", "-size", "512m", "-fs", "APFS", "-volname", &volume,
        ])
        .arg(&image));
    run(Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse"])
        .arg(&image));
    let root = PathBuf::from("/Volumes").join(&volume);
    let _mounted = MountedImage {
        mount_point: root.clone(),
    };
    assert!(root.is_dir(), "mounted at {}", root.display());

    for index in 0..40 {
        write_incompressible(
            &root.join(format!("old/build/part-{index}.o")),
            2_000_000,
            index,
        );
    }
    write_incompressible(&root.join("movie.mov"), 80_000_000, 101);
    write_incompressible(&root.join("keep/original.bin"), 20_000_000, 102);
    run(Command::new("/bin/cp")
        .arg("-c")
        .arg(root.join("keep/original.bin"))
        .arg(root.join("clone.bin")));
    write_incompressible(&root.join("keep/linked.bin"), 10_000_000, 103);
    fs::hard_link(root.join("keep/linked.bin"), root.join("link.bin")).unwrap();
    // SAFETY: `sync` has no preconditions.
    unsafe { libc::sync() };

    let tree = scanner::scan(ScanOptions::new(&root)).unwrap();
    let mut basket = Basket::new(Places::current().unwrap(), &root);
    for name in ["old", "movie.mov", "clone.bin", "link.bin"] {
        let path = root.join(name);
        let node = tree.find(&path);
        let size = node.map_or(0, |id| tree.allocated(id));
        basket.add(&path, node, Category::Chosen, size).unwrap();
    }
    let refused = basket.add(&root.join(".Trashes/x"), None, Category::Chosen, 0);
    assert!(refused.is_err(), "the volume's Trash can't be added");

    let started = Instant::now();
    let measurement = scanner::measure(&basket.paths()).unwrap();
    let measure_time = started.elapsed();
    let will_free = measurement.freeable;

    let before = settled_available(&root);
    let outcome = cleanup::move_to_trash(
        basket.items(),
        basket.places(),
        basket.scan_root(),
        &RunningApps::default(),
        &cleanup::Inventory::known(Vec::<String>::new()),
    );
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let trashed: Vec<PathBuf> = outcome
        .moved
        .iter()
        .map(|moved| {
            moved
                .trashed
                .clone()
                .expect("the Trash reports where items went")
        })
        .collect();
    let failures = cleanup::delete_permanently(&trashed);
    assert!(failures.is_empty(), "{failures:?}");
    let after = settled_available(&root);
    let gained = after.saturating_sub(before);

    println!(
        "estimate {} · will free {will_free} (shared {}) in {measure_time:?} · gained {gained} · difference {} bytes",
        basket.estimate(),
        measurement.shared(),
        gained as i64 - will_free as i64,
    );
    assert!(will_free >= 160_000_000, "the folder and movie are freed");
    assert!(
        will_free < basket.estimate(),
        "the clone and the hard link free nothing while their twins remain"
    );
    let difference = gained.abs_diff(will_free);
    assert!(
        difference * 100 <= will_free,
        "gained {gained} bytes, expected {will_free} within 1%"
    );
    assert!(root.join("keep/original.bin").exists() && root.join("keep/linked.bin").exists());
}
