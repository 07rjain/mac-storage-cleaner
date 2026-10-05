# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Product requirements document (`PRD.md`) covering scanning, visualization, cleanup, Sentry error reporting and release process.
- Cargo workspace with the `app` (GPUI window) and `telemetry` (Sentry) crates.
- Main window with the app name, version, and a "Send crash reports" toggle. The setting is saved in `~/Library/Application Support/mac-storage-cleaner/settings.json`.
- Sentry error reporting, on by default. Panics and error-level `tracing` events are reported; info and warn events become breadcrumbs.
- Scrubbing of every outgoing Sentry event and breadcrumb: file paths are replaced with `<path>`, user and volume names are removed from code locations, and the hostname is dropped. Events that can't be scrubbed are not sent.
- `--test-panic` flag that sends a panic containing a fake path, to check scrubbing end to end.
- MIT `LICENSE`, `THIRD_PARTY_NOTICES.md`, and the GPUI Apache-2.0 license text.
- CI workflow for formatting, Clippy, tests and a release build on macOS.
- `scanner` crate: parallel directory walk with `getattrlistbulk` into a compact tree, with live progress and cancellation. About 200,000 entries per second on Apple silicon.
- The scanner never downloads iCloud files: it turns off dataless-file materialization before starting and refuses to scan if that fails. Dataless folders are not opened.
- Hard-linked files and unedited APFS clones are counted once. Edited clones are counted in full, and their total is reported.
- The scan stays on one volume, never follows symlinks, and flags unreadable folders instead of counting them as empty.
- `volumes` crate: mounted volumes, per-volume used space, purgeable space, local snapshot count, and startup-disk accounting (scanned, not scanned, other volumes) that adds up to the container's used space.
- `msc-scan` command-line harness for benchmarks and accuracy checks. Without a path it scans the startup disk's Data volume and prints the accounting.
- Accuracy tests for nested sizes, hard links, clones, sparse files, symlinks, unreadable folders and cancellation, plus an opt-in test on a fresh APFS disk image.

- Main window (M2): the scan starts on the startup disk at launch and is drawn live.
  - Capacity bar with used, purgeable and available space and the local snapshot count.
  - Sunburst chart of up to six levels. Click a folder to zoom in, click the center to go back. Slices under half a degree are merged into "Smaller items".
  - At the top of the startup disk, "macOS and other volumes" and "Not measured" slices make the chart add up to the disk's used space. "Not measured" links to the Full Disk Access settings.
  - Side list of the current folder, largest first, with size bars and notes for unreadable folders, other volumes, iCloud placeholders, hard links and clones. It stays in sync with the chart on hover and selection.
  - Breadcrumb with a menu of sub-folders after each segment.
  - Folders still being counted are drawn muted and show "≥" sizes until final.
  - Scan the startup disk, the home folder, or any folder (Scan menu, toolbar, or `mac-storage-cleaner <folder>`); Rescan and Stop.
  - Keyboard: arrows move the selection, Return or → opens, ← or ⌘↑ goes up, ⌥⌘R reveals in Finder, ⌘R rescans, ⌘. stops.
  - "Send Crash Reports" moved to the app menu, as a checked item.
- `volumes::volume_name` for the name Finder shows, for example "Macintosh HD".

### Changed
- iCloud features (iCloud-only ring, evict and download) are deferred to a future release. `PRD.md` updated, including M1 and M2 benchmark and accuracy results.
- The scanner's tree can be read while the scan runs (`ScanHandle::tree`), with totals kept current and `Tree::is_settled` telling whether a folder's total is final.
- Finished listings now wait in a bounded queue, so peak memory no longer depends on how far the workers get ahead.

### Dependencies
- GPUI and `gpui_platform` pinned to Zed commit `279fe070bb389b79652e52065b2f001edcc0b11b` (2026-10-04).
- Rust toolchain pinned to 1.99.0.
- `sentry` 0.49.
- `parking_lot` 0.12 for the scanner's shared tree.
