# Mac Storage Cleaner

A native macOS app that shows where disk space is going and helps you reclaim it safely. Built in Rust with [GPUI](https://www.gpui.rs/).

**[Download the latest DMG](https://github.com/07rjain/mac-storage-cleaner/releases/latest)** · [Changelog](CHANGELOG.md)

Apple silicon, macOS 14 or later.

## Install

1. Download `Mac-Storage-Cleaner-<version>.dmg` from [Releases](https://github.com/07rjain/mac-storage-cleaner/releases).
2. Open the disk image and drag **Mac Storage Cleaner** to Applications.
3. The app is signed ad hoc and not notarized. On first launch, right-click the app and choose **Open**, or use System Settings › Privacy & Security › **Open Anyway**.
4. Grant Full Disk Access when asked if you want protected folders measured. Each ad-hoc build looks like a new app to macOS, so access has to be granted again after updating.

## What it does

- Scans the startup disk, home folder, or a folder you pick. The first chart appears while the scan is still running.
- **Sunburst** and **treemap** charts of the current folder, kept in sync with a largest-first list. Switch with ⌥⌘1 / ⌥⌘2.
- After a scan, the tree updates when files change, without a full rescan.
- The list notes folders that grew or shrank by at least 1 MB since the last scan of the same place.
- Cleanup suggestions (old installers, Xcode data, caches, build folders) go into a review basket. Nothing is deleted without confirmation; items move to the Trash.
- Scanning never downloads iCloud files.

## Privacy

Crash reports are on by default and can be turned off on first launch or in Settings. Reports do not include file names, paths, or user names. Native crashes record instruction addresses and library IDs only.

## Build from source

Needs a recent Rust toolchain (see `rust-toolchain.toml`) and Xcode's Metal toolchain.

```sh
cargo test --workspace
./scripts/bundle.sh
```

The DMG lands in `dist/`. `scripts/bundle.sh` reads `SENTRY_DSN` from the environment or `.env` if you want crash reporting in that build.

## License

[MIT](LICENSE)
