# PRD: Mac storage visualizer and cleaner (GPUI)

Status: Draft v0.1 · Owner: Rishabh · Date: 2026-10-05

## 1. Summary

A native macOS app that shows where every byte on the disk is going and helps the user reclaim space safely. It is built in Rust with GPUI from Zed.

The product has three promises:

1. **Fast.** The first results appear in under a second, and a typical home folder scans in seconds.
2. **Accurate.** The whole-disk chart adds up to what Disk Utility reports. Scanning never downloads iCloud files. "Will free" numbers count clones and hard links honestly.
3. **Clear and safe.** One sunburst chart explains the disk, including the space Apple's Storage settings calls "System Data". Nothing is deleted without review, and everything goes to the Trash.

## 2. Problem

- Apple's Storage settings show a category bar with a large, unexplained "System Data" bucket, and give no map of where space actually is.
- Paid cleaners such as CleanMyMac lead with one-click cleaning that removes a lot at once and explains little.
- Good disk maps (DaisyDisk, GrandPerspective) don't explain purgeable space, snapshots or iCloud consistently. Most open-source tools are terminal-only.
- Scanning iCloud Drive the naive way can download evicted files and fill the disk while "measuring" it.

## 3. Target users

- **Primary:** a non-technical Mac user who sees "Your disk is almost full" and wants to know why, and what is safe to remove.
- **Secondary:** developers whose disks fill with Xcode data, package caches, `node_modules`, `target` folders and Docker images.

## 4. Goals and non-goals

### Goals (v1)
- Show the whole startup disk as one chart that adds up to the container's used space.
- Let the user drill into any folder and find the largest items in a few clicks.
- Never download iCloud files while scanning.
- Offer a short list of safe cleanup suggestions, each with a reason.
- Reclaim space through a review basket that moves items to the Trash.
- Report errors to Sentry without sending file paths or file names.
- Keep a changelog for every release.

### Non-goals (v1)
- Matching or recomputing Apple's "System Data" number.
- Privileged helper, root scans, kernel extensions, or scanning other users' home folders.
- Deleting local snapshots or forcing a purge.
- Any iCloud feature (deferred to Future, see section 13): showing what is only in iCloud, evicting or downloading files, toggling Optimize Mac Storage, or analyzing the iCloud account or quota.
- Opening up the Photos library, iOS backups, Mail or Messages data.
- Uninstaller, duplicate finder, "optimize", memory cleaning, malware scanning.
- Permanent delete as a default action.
- Mac App Store distribution (sandboxing blocks the scanning this product needs).
- Menu bar or background monitoring.

## 5. What we take from other projects

| Area | Source | What we take |
|---|---|---|
| Scanner speed | Petal (MIT), Layland | `getattrlistbulk` + `openat` + parallel workers; flat arena tree |
| iCloud safety | Apple TN3150 | `setiopolicy_np` with dataless materialization off; `SF_DATALESS` checks |
| Whole-disk accounting | DaisyDisk, Petal | Slices for other volumes, purgeable, snapshots, unreadable |
| Sunburst rendering | Petal (MIT) | Polar hit testing and merged small slices as a design reference. Our implementation is original, so no code is attributed |
| Navigation | disktree (MIT) | Breadcrumb with sibling dropdown; Full Disk Access detection |
| Live scan display | Petal, Mole | Muted while counting, "≥ size" while partial |
| Review basket | DaisyDisk (design only) | Collect, review, confirm; refused system roots |
| Delete primitive | Petal, Mole | Move to Trash, never unlink by default |
| Safety rules | Mole (design only, GPL-3) | Allow-list, skip running apps' caches, 7-day and Git rules |
| Treemap (v1.1) | disktree (MIT), GrandPerspective | Squarified treemap, color by file type |

Licensing rules:
- MIT code (Petal, disktree) may be copied or adapted. Each adapted file keeps its original copyright and MIT notice, and is listed in `THIRD_PARTY_NOTICES.md`.
- GPUI is Apache-2.0. Its license and any NOTICE content go in `THIRD_PARTY_NOTICES.md` and the app's About window.
- Mole is GPL-3 and DaisyDisk is closed source. We copy ideas from them, never code.
- Petal and disktree were days old at the time of research, so they are references, not dependencies. Their accuracy claims must be re-verified by our own tests.

## 6. User experience

### 6.1 First launch
1. A one-screen explanation of what the app reads and that nothing is deleted without review.
2. A Full Disk Access request with a button that opens the right Settings pane (`x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`). The app re-checks access when it returns to the foreground. The user can skip this; protected folders then appear as "Not measured".
3. A notice that crash reports are sent without file names, with a toggle to turn them off (see section 9).
4. The scan starts automatically on the startup disk.

### 6.2 Main window

```
┌───────────────────────────────────────────────────────────────┐
│ Macintosh HD   [■■■■■■■■■■■■■■■□□□□]  412 GB used · 82 GB free │
│                purgeable 14 GB · 3 snapshots                  │
├───────────────────────────────────────────┬───────────────────┤
│ ~ / Library / Developer ▾                 │ Largest items     │
│                                           │ DerivedData 38 GB │
│              ( sunburst )                 │ CoreSimulator 21  │
│                                           │ Archives 9 GB     │
│                                           │ …                 │
├───────────────────────────────────────────┴───────────────────┤
│ Suggestions: Xcode DerivedData 38 GB · Old installers 6 GB …  │
│ Basket: 3 items · will free 44.1 GB              [Review…]    │
└───────────────────────────────────────────────────────────────┘
```

- **Capacity bar (top, always visible):** used, purgeable and free space for the selected volume. After cleanup it shows the before and after numbers.
- **Sunburst (center):** the center is the current folder; each ring is one level deeper. Clicking a slice zooms in, and clicking the center goes up. At most 6 rings are shown. Slices narrower than about 0.5° are merged into one "Smaller items" slice.
- **Accounting slices at the root:** other APFS volumes, purgeable space, local snapshots, and "Not measured" (protected or unreadable areas). The "Not measured" slice offers the Full Disk Access prompt.
- **Files stored only in iCloud** count at their local size, which is close to zero. A separate iCloud view is deferred to Future.
- **Side list:** the children of the current folder, sorted by size, kept in sync with the chart on hover and selection.
- **Breadcrumb:** the path, with a dropdown on each segment for jumping to sibling folders.
- **Live scan:** folders are muted while being counted and show "≥ 12.4 GB". They switch to full color when final.
- **Context menu (right-click on a slice or row):** Add to basket (disabled with the reason for refused paths), Open for folders, Quick Look, Reveal in Finder.

### 6.3 Suggestions
Cards labelled "Safe to delete" or "Review first", each with a size and item count. Clicking a "Safe to delete" card adds its items to the basket. Every card opens a list with the reason, what was skipped and why, and a per-item Add button. See section 8 for categories.

### 6.4 Review basket
1. The user drags slices or list items in, or adds a suggestion.
2. The basket shows every item, its size, and **Will free**, with clones and hard links counted once.
3. One confirmation moves everything to the Trash.
4. The app shows the real free-space change. If the change is smaller than expected, it explains why (snapshots still hold the data, or shared clone blocks).
5. Emptying the Trash is a separate, explicit button that shows the Trash size.

## 7. Functional requirements: scanning and accuracy

### 7.1 Scope
- **Default:** the startup disk. The app walks only the Data volume and gives every other APFS volume in the container an exact slice.
- **Optional:** the home folder only, or a folder or external volume picked by the user.
- Stay on one file system per walk. Don't follow symlinks. Firmlinks are handled so nothing is counted twice.

### 7.2 Scanner
- Read directories with `getattrlistbulk`, opening children with `openat` from the parent descriptor. Use a work-stealing pool of worker threads.
- Per entry, request: name, object type, device, file ID, flags, mount status, link count, allocated size, logical size, clone ID and extended flags (`FSOPT_ATTR_CMN_EXTENDED`), plus a per-entry error.
- Only the coordinator thread builds the tree; workers only list directories.
- Don't rely on `ATTR_DIR_ENTRYCOUNT`, because APFS reports 0 for some firmlinked or sealed folders.
- Store results in a flat arena (parent index, first child, next sibling, sizes, flags) with names in a separate string arena.
- Stream progress to the UI in batches. The UI recomputes the layout at most every 100 ms.

### 7.3 Size rules
- The size counted toward disk use is the **allocated** size.
- Logical size is stored too, and shown on hover for sparse files (for example `Docker.raw`).
- Hard links: entries with link count > 1 are counted once per `(device, file ID)`.
- APFS clones are handled during the main scan, because apps such as WhatsApp clone tens of thousands of files and counting each copy overstates use several times over:
  - Unedited clones (`EF_SHARES_ALL_BLOCKS`) with the same `ATTR_CMNEXT_CLONEID` are counted once.
  - Edited clones (`EF_MAY_SHARE_BLOCKS` only) get a new clone ID and can't be matched to the file they share blocks with, so they are counted in full, like `du` and Finder. Their count and size are reported as the upper bound of the overcount.
  - `ATTR_CMNEXT_PRIVATESIZE` is not requested during the scan: it slowed the walk by about 45%. **Will free** reads it only for basket items (`scanner::measure`).

### 7.4 Dataless files (iCloud-safe scanning)
- Before any scanning, the scanner process sets `setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_PROCESS, IOPOL_MATERIALIZE_DATALESS_FILES_OFF)`. If this fails, the scan does not start.
- Entries with `SF_DATALESS` are flagged; their allocated size (close to zero) counts toward local use, and their logical size is kept for the future iCloud view.
- Dataless directories are not descended into. `EDEADLK` from a read is treated as "dataless, skip", never as a failure.
- The app never calls `brctl`, `fileproviderctl`, or any download or evict API.

### 7.5 Volume accounting
- Volume capacity and free space come from `statfs`.
- Per-volume used bytes come from `getattrlist` with `ATTR_VOL_SPACEUSED`.
- Purgeable = "available for important usage" (`NSURLVolumeAvailableCapacityForImportantUsageKey`) minus "available" (`NSURLVolumeAvailableCapacityKey`).
- Local snapshots are listed by count and date only. Their size is never estimated.
- "Not measured" = container used space − (scanned bytes + other volumes + purgeable). Never shown as negative; any mismatch is logged.

### 7.6 Permissions
- Full Disk Access is detected by checking whether a TCC-protected path is readable (heuristic used by disktree), and re-checked on app activation.
- Folders that return `EPERM` or `EACCES` are shown as "Not measured", never as 0 bytes.

### 7.7 Refresh
- v1: a Rescan button, plus rescanning a single folder from the context menu.
- v1.1: incremental refresh with FSEvents. The scanner records the current FSEvents event ID before the walk so this can be added without changing the data model.

## 8. Functional requirements: cleanup

### 8.1 Suggestion categories (v1)

| Category | Default in basket | Rule |
|---|---|---|
| Old installers in Downloads and Desktop (`.dmg`, `.pkg`, `.xip`, `.iso`) | Preselected | Older than 90 days |
| Xcode DerivedData | Preselected | Xcode not running |
| Xcode DeviceSupport (iOS, watchOS, tvOS, visionOS, macOS) | Preselected | Keep the newest folder per platform |
| Xcode Archives | Review only | Never preselected |
| Package manager caches (npm, yarn, pnpm, pip, Cargo registry, Homebrew downloads) | Review only | Owning tool not running |
| Project build folders (`node_modules` and `dist` next to `package.json`, `target` next to `Cargo.toml`, `.build` next to `Package.swift`) | Review only | At least 10 MB, not modified in 7 days, and not tracked by Git |
| `~/Library/Caches/<app>` | Review only | At least 1 MB, owning app not running, never `com.apple.*` |
| `~/Library/Logs` | Review only | None |
| Large files (≥ 1 GB) | Review only | Never preselected |

Suggestions never overlap: an item claimed by one category is not offered by another.

### 8.2 Show, but don't clean
Photos library, iOS backups, Mail downloads, Messages attachments, `Docker.raw` and VM images. Each shows its size and a button that opens the right app or settings pane.

### 8.3 Safety rules
- Suggestions come from an allow-list. Anything the app can't classify is never suggested.
- Personal files are never preselected.
- Refused at all times: `/System`, `/Library`, `/private`, `/Applications` and `~/Applications` (v1), other users' folders, the home folder itself and its standard folders (Desktop, Documents, Downloads and so on), `~/Library` and its direct children, the Trash, iCloud Drive and other cloud storage folders, keychains and preferences, Mail, Messages and Photos data, app containers themselves, any package internals (`.photoslibrary`, `.app`, `MobileSync/Backup` contents), external volume roots and their system folders, any dataless entry, and any symlink that resolves outside the scanned area.
- Permanent deletion only acts on items inside a Trash folder.
- A cache is skipped if its owning app is running.
- At confirmation time, each path is re-checked (still exists, same file ID, same type) before it is moved.
- Items are moved with `NSFileManager trashItemAtURL:resultingItemURL:error:`. A partial failure moves what it can and lists what failed.
- Every cleanup writes a local operation log (time, category, count, bytes; paths kept locally only), viewable from the app.

## 9. Error handling and Sentry

### 9.1 Keys
- The value currently in `.env` as `sentry_api` is a **Sentry user auth token** (`sntryu_…`). It is a secret for the Sentry API and `sentry-cli` (uploading debug symbols, creating releases). It must **never** be compiled into the app or committed.
- The app reports events with the project's **DSN**, read at build time from `SENTRY_DSN`. If `SENTRY_DSN` is not set, Sentry is disabled and the app runs normally.
- Proposed `.env` names: `SENTRY_DSN` (build), `SENTRY_AUTH_TOKEN` (CI symbol upload; `sentry-cli` reads it natively), `SENTRY_ORG`, `SENTRY_PROJECT`.

### 9.2 What is captured
- Rust panics, through the `sentry` crate's panic integration.
- `tracing` events at error level as Sentry events, and info or warn level as breadcrumbs (`sentry-tracing`).
- Handled errors that indicate bugs: scanner invariant violations, accounting mismatches over 1%, unexpected `getattrlistbulk` errors, failed Trash moves.
- Not captured: expected conditions such as `EPERM` on protected folders, `EDEADLK` on dataless files, or the user cancelling.
- Releases are tagged `<app-id>@<version>`, matching the changelog version. Environments: `development` and `production`.
- Debug symbols (dSYM) are uploaded in the release pipeline with `sentry-cli` so stack traces are readable.
- Native crashes outside Rust panics (for example in Objective-C or C calls) are not captured by the Rust SDK alone. Adding native crash capture is a v1.1 item.

### 9.3 Privacy
A disk tool sees file names that can identify people and projects, so:
- `send_default_pii` is off.
- A `before_send` hook removes all file paths, file names, volume names, and user names from messages, exception values, breadcrumbs and extra data. Paths are replaced with a category token such as `<home>/<redacted>`.
- Events contain only app version, macOS version, CPU architecture, error type, sizes rounded to the nearest GB, and counts.
- Crash reporting is **on by default**. First launch states this plainly, and the user can turn it off there or in Settings at any time. Turning it off takes effect immediately: no event captured after that point is sent.
- No analytics or usage tracking in v1.

## 10. Non-functional requirements

### 10.1 Performance targets (Apple silicon, internal SSD)
- First chart drawn within 500 ms of starting a scan.
- Scan throughput of at least 200,000 entries per second with a warm cache.
- The UI stays responsive (no frame over 16 ms) during a scan.
- Memory at most about 150 MB for 1 million entries.
- Basket "Will free" computed in under 1 second for up to 10,000 items.

M1 measurements (2026-10-05, this development Mac, warm cache, `msc-scan`):

| Scan | Entries | Time | Throughput | First results | Peak memory |
|---|---|---|---|---|---|
| Home folder | 3.5 million | 16.8–17.3 s | 203,000–209,000 per second | 24 ms | 197–224 MB |
| Whole Data volume | 4.45 million | 22.6 s | 197,000 per second | 25 ms | 256–399 MB |

Throughput is at the target. Memory is about 60–90 MB per million entries. Its spread between runs comes from results queued between workers and the tree builder, which M2 should bound.

M2 measurements (2026-10-05, same Mac):
- The tree is now readable during the scan, with live totals. That costs about 5% throughput: home folder 17.2–18.4 s, about 190,000–204,000 entries per second; whole Data volume 23–25 s.
- The listing queue is bounded at 64. Peak memory for the whole Data volume is 267 MB (was 256–399 MB), and 174–223 MB for the home folder.
- The first chart appears within one 100 ms refresh of starting the scan.
- Building the rows and chart layout for one frame takes 0.4 ms at the top of a 3.5-million-entry home folder, and 0.1 ms or less deeper down (`layout_time_on_the_home_folder`, an opt-in test). Painting was not timed separately.

### 10.2 Accuracy acceptance
- Whole-disk chart total within 1% of Disk Utility's container used space on a test Mac.
- Zero iCloud downloads during a scan: an evicted test file stays dataless before and after.
- Hard-linked test files counted once; a sparse test file counted at allocated size.
- On a throwaway APFS volume with no snapshots, the actual free-space change after cleanup is within 1% of "Will free".

M1 results (2026-10-05):
- Whole Data volume without Full Disk Access: 146.7 GB scanned out of 158.7 GB used. The other 12.0 GB is in 211 protected folders and file-system metadata, shown as "Not measured". The chart adds up exactly to container use: scanned + not measured + other volumes.
- Fresh 256 MB APFS disk image: scanned total within 32 KB of the volume's used space.
- Home folder totals match `du` to within 0.02% when clone handling is off. With it on, 207 GB of WhatsApp clones count once.
- 2,384,434 dataless items reported in every run, before and after scanning, so the scan didn't download any of them.
- Not yet measured: the whole-disk gap with Full Disk Access granted (M4 onboarding).

M3 results (2026-10-05):
- Fresh 512 MB APFS disk image (`free_space_gained_matches_will_free`, an opt-in test). The basket held a folder, a large file, an edited clone of a file that stays, and a hard link to a file that stays. Will free said 160,120,832 bytes; emptying the Trash freed 160,133,120 bytes, 12 KB (0.008%) more. Counting allocated sizes would have estimated about 190 MB. Measuring took under 1 ms.
- On this Mac's startup disk, a 21-item basket of logs and large files was measured as 6.25 GB to free, with 5.92 MB shared with files outside the basket.
- Refused paths can't be added to the basket (`refused_paths_cannot_be_added`, plus UI tests for the right-click menu and keyboard).

### 10.3 Platform
- macOS 14 (Sonoma) or later. Sonoma moved iCloud Drive to File Provider, and DaisyDisk reports clone detection needs macOS 14.
- Apple silicon (`aarch64-apple-darwin`) only in v1.
- Unsandboxed, hardened runtime, distributed as a DMG.
- 0.1.0 is ad-hoc signed and not notarized, because there is no Apple Developer membership yet. The first launch needs right-click › Open, or System Settings › Privacy & Security › Open Anyway. Developer ID signing and notarization come when a membership is available.
- Ad-hoc signatures change with every build, so macOS treats each build as a new app and Full Disk Access must be granted again after updating.

### 10.4 Accessibility
- Every chart slice is reachable from the side list with the keyboard.
- Colors meet contrast guidelines; size is never shown by color alone.
- Light and dark appearance follow the system.

## 11. Technical architecture

### 11.1 GPUI
- Use upstream GPUI from `https://github.com/zed-industries/zed/tree/main/crates/gpui` as a Git dependency **pinned to a specific commit** (`rev = "<sha>"`), never a moving branch.
- Upgrade the pin on purpose, in its own change, with a changelog entry.
- Risk: upstream GPUI is pre-1.0 and changes with Zed. If the pinned commit fails to build standalone, fall back to the `gpui-ce` fork that Petal uses, and record the decision in the changelog.

### 11.2 Crates (Cargo workspace)

| Crate | Responsibility | Depends on GPUI |
|---|---|---|
| `app` | Window, views, sunburst element, onboarding, settings | Yes |
| `scanner` | `getattrlistbulk` walk, arena tree, dataless policy | No |
| `volumes` | Capacity, per-volume used, purgeable, snapshot list | No |
| `cleanup` | Suggestion rules, basket, re-check, Trash, operation log | No |
| `telemetry` | Sentry setup, `before_send` scrubbing, opt-in state | No |
| `scan-cli` | `msc-scan` harness: benchmarks, accuracy checks, startup-disk accounting | No |

- System calls through `libc`; Foundation calls (`NSURL` resource keys, `NSFileManager`) through `objc2` and `objc2-foundation`.
- `scanner`, `volumes` and `cleanup` have a small command-line harness for benchmarks and accuracy tests without the UI.

### 11.3 Data flow
1. `scanner` workers write into the arena and send progress batches over a channel.
2. The `app` view polls the channel on a timer, rebuilds the visible sunburst layout from the current focus folder, and repaints.
3. Clicks are hit-tested with polar math (angle and radius) instead of per-slice geometry.
4. `cleanup` reads the arena for suggestions and the basket, and re-checks paths on disk before moving.

## 12. Changelog and release process
- `CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) with [Semantic Versioning](https://semver.org/).
- Every change that affects users, dependencies, the GPUI pin, Sentry setup or safety rules adds a line under `## [Unreleased]`.
- A release moves `Unreleased` into a dated version section. The same version is used for the app bundle and the Sentry release.
- The app's About window shows the version and links to the changelog.
- `THIRD_PARTY_NOTICES.md` is updated in the same change as any copied or adapted MIT or Apache code.

## 13. Milestones

| Milestone | Scope | Exit criteria |
|---|---|---|
| M0 Foundation | Workspace, pinned GPUI window, Sentry with scrubbing, changelog, notices, CI build | Empty window launches; a test panic reaches Sentry with no paths |
| M1 Scanner | `scanner` + `volumes` + harness | Accuracy acceptance tests in 10.2 pass; throughput target met |
| M2 Visualization | Capacity bar, sunburst, side list, breadcrumb, live scan | A full disk is explorable with mouse and keyboard. Done 2026-10-05: interaction tests drive the view with simulated clicks, mouse moves and keystrokes |
| M3 Cleanup | Suggestions, basket, Will free, Trash, operation log | Will-free test passes; refused paths cannot be added. Done 2026-10-05, see 10.2 |
| M4 Release 0.1.0 | Onboarding, Full Disk Access flow, settings, ad-hoc signing, DMG, dSYM upload | DMG installs and runs on macOS 14 or later (notarization waits for a Developer ID) |
| v1.1 | FSEvents refresh, treemap tab, native crash capture, scan comparison | Separate PRD update |
| Future: iCloud | "In iCloud, not on this Mac" ring at full logical size, iCloud Drive breakdown, evict and download actions | Separate PRD update |

## 14. Risks
- **Upstream GPUI churn:** pin a commit; keep GPUI out of the non-UI crates so a framework change only touches `app`.
- **Accounting mismatches on unusual setups** (multiple containers, FileVault, external boot): log and show "Not measured" rather than wrong numbers.
- **Full Disk Access detection is a heuristic:** fall back to "Not measured" counts if detection is wrong.
- **Deleting something the user needed:** Trash-only, allow-list, re-check at confirm time, no preselected personal files.
- **Reference projects are very new:** verify every borrowed accuracy claim with our own tests.

## 15. Decisions and open questions

Decided (2026-10-05):
- License: MIT.
- Crash reporting: on by default, with an opt-out.
- Architecture: Apple silicon only in v1.
- GPUI: upstream Zed, pinned commit.
- iCloud features: deferred to Future. Scanning stays iCloud-safe and never downloads files.
- APFS clones: unedited clones are counted once during the main scan; edited clones are counted in full.
- App name "Mac Storage Cleaner", bundle ID `io.github.07rjain.mac-storage-cleaner`.
- 0.1.0 ships as an ad-hoc signed DMG without notarization.

Open:
1. Whether the app is free, paid, or open source.
2. Whether `/Applications` should be cleanable in a later version (needs an admin prompt).

## 16. Research references
- Apple TN3150, Getting ready for dataless files: https://developer.apple.com/documentation/technotes/tn3150-getting-ready-for-data-less-files
- Apple, Checking volume storage capacity: https://developer.apple.com/documentation/foundation/checking-volume-storage-capacity
- DaisyDisk guide: https://daisydiskapp.com/guide/4/en/
- Petal: https://github.com/henrydennis/petal
- disktree: https://github.com/tobi/disktree
- Mole: https://github.com/tw93/Mole
- GrandPerspective: https://grandperspectiv.sourceforge.net/
- Eclectic Light, Storage settings explainer: https://eclecticlight.co/2025/12/20/explainer-storage-settings/
- GPUI: https://github.com/zed-industries/zed/tree/main/crates/gpui
