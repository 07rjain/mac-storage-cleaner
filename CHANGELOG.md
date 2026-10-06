# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.4] - 2026-10-06

### Added

- Leftover files for bundle IDs that were not found under Applications on this Mac. The card is review-only and can include a third-party app container only while a fresh check still shows that app is absent. Group Containers, LaunchAgents, and Preferences stay refused.

## [0.2.3] - 2026-10-06

### Added

- Icicle chart, an expandable tree, and file-type bars. Switch from the View menu or the control next to the breadcrumb: Icicle (⌥⌘3), Tree (⌥⌘4), File Types (⌥⌘5). In the tree, the arrow or the Right arrow key expands a folder; Right arrow again opens it.

## [0.2.2] - 2026-10-06

### Added
- Check for Updates in the app menu and the About window. It reads the latest GitHub release and, when a newer one exists, opens that disk image. The repository has to be public; no token is built into the app.

## [0.2.1] - 2026-10-06

### Fixed
- Live refresh no longer crashes after the scanned folder changes. Updating the chart was keeping every previous copy of the tree until the name storage overflowed and the app aborted.

## [0.2.0] - 2026-10-06

### Added
- Treemap chart: nested rectangles sized by space and colored by file type, with a legend. Switch from the sunburst with View › As Treemap (⌥⌘2), View › As Sunburst (⌥⌘1), or the control next to the breadcrumb. The choice is saved.
- After a scan, the app watches the folder with FSEvents and updates changed folders in place, without a full rescan.
- Native crash reports (segmentation faults and similar) are written as instruction addresses and library IDs only, then sent to Sentry on the next launch. File paths, user names and memory are not recorded. Rust panics are unchanged.
- The list and status bar show what grew or shrank by at least 1 MB since the last scan of the same place.

### Changed
- CI uploads the DMG with `actions/upload-artifact@v5`.

## [0.1.1] - 2026-10-05

### Added
- Rescan This Folder in the right-click menu for folders. Only that folder is scanned again, and the chart, list, suggestions and basket update without a full scan.
- After moving items to the Trash, the result shows how much the Trash holds in all, since emptying it in Finder deletes everything in it.
- VoiceOver labels: buttons, check boxes, menus, the capacity bar, the chart, list rows (name, size, notes, and whether the item is in the basket), suggestion cards, the basket, the cleanup result, history and the status bar. Arrow keys in the list move the VoiceOver cursor with the selection.
- `msc-scan --list-unreadable N` lists folders the scan couldn't read.
- Opt-in benchmarks for Will free on 10,000 items and for frame time while scanning the home folder.

### Changed
- Text and button colors meet the WCAG AA contrast ratio (4.5:1) in light and dark appearance, checked by a test. Muted text, accent text, the selected row and the "Safe to delete" and "Review first" colors changed slightly; primary and destructive buttons use darker fills behind their white labels.

### Fixed
- Will free took about 5 seconds for 10,000 items, because nested items were removed in quadratic time. It now takes about 80 ms.

## [0.1.0] - 2026-10-05

First release. Apple silicon, macOS 14 or later. The app is signed ad hoc and not notarized, so the first launch needs System Settings › Privacy & Security › Open Anyway.

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

- Cleanup (M3):
  - Suggestion cards under the chart, appearing after a scan finishes:
    - "Safe to delete": old installers in Downloads and Desktop, Xcode DerivedData, and old Xcode DeviceSupport folders (the newest per platform is kept).
    - "Review first": Xcode archives, package manager caches, project build folders, app caches, logs, and large files.
    - Clicking a "Safe to delete" card adds its items to the basket. Every card opens a list where items can be added one by one.
  - Caches and DerivedData are skipped while the app or tool that owns them is running. Build folders are skipped if Git tracks them or they changed in the last 7 days.
  - Cards for places that are shown but not cleaned: Photos library, iPhone and iPad backups, Mail downloads, Messages attachments, the Docker disk and virtual machines. Each opens the right app, settings pane or Finder window.
  - Basket: add items by dragging list rows onto the basket bar, from the right-click menu, with ⌘⌫, or from a suggestion.
    - Paths that are never safe to clean are refused with a reason, for example system folders, apps, the home folder and its standard folders, iCloud Drive, package contents, and iCloud placeholders.
    - Adding a folder replaces items already inside it.
  - **Will free**, measured on disk:
    - Clones count only their private blocks.
    - Hard links count only when every link is in the basket.
    - Data shared with files outside the basket is reported separately.
  - Move to Trash:
    - Each item is checked again just before it moves: same safety rules, same file, and not in use by a running tool.
    - What moved leaves the chart and the basket, what failed is listed with a reason, and the real free-space change is shown next to the estimate.
    - "Delete from Trash" removes only the items this cleanup moved there.
  - Operation log in `~/Library/Application Support/mac-storage-cleaner/cleanup-log.jsonl`, with paths kept only on this Mac, viewable from Clean › Cleanup History.
  - Right-click menu on chart slices and list rows: Add to Basket or Remove, Open (for folders), Quick Look, Reveal in Finder.
  - Quick Look with Space or ⌘Y. ⌘B opens the basket. New Clean menu.
- `cleanup` crate: suggestion rules, path safety checks, basket, running-app detection, Trash moves and the operation log, with tests on a fake home folder.
- `scanner::measure`, which reads APFS private sizes for Will free. `Tree::find` and `Tree::remove`, so cleaned items leave the tree without a rescan.
- Opt-in Will free test on a fresh APFS disk image: the free-space change was within 12 KB of the estimate (0.008%).

- Release (M4):
  - Welcome window on first launch. It explains what the app reads and that nothing is removed without review, shows whether Full Disk Access is on (checked again each time the window comes back to the front), links to its settings pane, and has the crash-report toggle. The scan starts when the user clicks Start Scanning.
  - Settings window (⌘,): crash reports, Full Disk Access status, and the cleanup log in Finder.
  - About window with the version, changelog, license and third-party notices. The documents ship inside the app.
  - The main window suggests a rescan when Full Disk Access is turned on while the app is open.
  - App bundle `Mac Storage Cleaner.app` (bundle ID `io.github.07rjain.mac-storage-cleaner`) with an icon, signed ad hoc with the hardened runtime, in a DMG.
  - `scripts/bundle.sh` builds the app and DMG, with the Sentry DSN from the environment or `.env`. `scripts/upload-dsym.sh` creates the Sentry release and uploads the debug symbols. `scripts/make-icon.swift` draws the icon.
  - Release builds keep line tables in a separate dSYM, so Sentry stack traces show file and line.

### Changed
- iCloud features (iCloud-only ring, evict and download) are deferred to a future release. `PRD.md` updated, including M1 and M2 benchmark and accuracy results.
- The scanner's tree can be read while the scan runs (`ScanHandle::tree`), with totals kept current and `Tree::is_settled` telling whether a folder's total is final.
- Finished listings now wait in a bounded queue, so peak memory no longer depends on how far the workers get ahead.
- "Send Crash Reports" is one app-wide setting, shared by the app menu, the welcome window and Settings.

### Dependencies
- GPUI and `gpui_platform` pinned to Zed commit `279fe070bb389b79652e52065b2f001edcc0b11b` (2026-10-04).
- Rust toolchain pinned to 1.99.0.
- `sentry` 0.49.
- `parking_lot` 0.12 for the scanner's shared tree.
- `objc2` and `objc2-foundation` 0.3 for `NSFileManager` Trash moves and app bundle identifiers.
