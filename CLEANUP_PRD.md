# PRD: Cleanup, next

Status: Draft · Owner: Rishabh · Date: 2026-10-06 · Revised after review `task_1313eb8f-e01c-442f-a4f7-308e723f4f4b`

Leftovers (section 5), put back (section 6), project cards (section 7), and exact copies (section 8) are in the app. Screenshots and the extra project folders in section 7 are in the app as well. Mail downloads and the Photos library stay show-only.

This is the spec for the next cleanup work. The product PRD (`PRD.md`) still governs scanning, charts, Sentry, and the v1 cleanup that already shipped. Where this document is silent, those rules stand. Section 5.3 narrows a refusal in `PRD.md` section 8.3: one proven leftover container, rechecked when it is added and when it is moved.

## 1. Summary

The next cleanup should reclaim space a disk map does not make obvious, without becoming a one-click cleaner.

Four additions, in order:

1. Leftover files from apps that were not found on this Mac.
2. Put back, from Cleanup History, for items this app moved to the Trash.
3. One suggestion card per project for build folders that are already eligible.
4. Exact duplicate files, and only when deleting a copy would free private space.

Nothing here is preselected. Every removal is still a move to the Trash. Unknown files are still never suggested.

Group Containers and LaunchAgents are not in this version. A name match does not prove who owns them.

## 2. Why this, and not a broader cleaner

Paid cleaners lead with a single review list, an uninstaller, and extras (malware, memory, updaters). Disk maps lead with a chart and a basket the user fills by hand. This app already has the chart, the basket, an allow-list, and a Will free number that counts clones and hard links once.

What a buyer still cannot do here:

- See support files left behind after an app was dragged to the Trash.
- Restore a cleanup after the result screen is gone. The Trash URL is kept on `Moved` for the current Empty Trash action and is not written to the history log.
- See `node_modules`, Cargo `target`, and Swift `.build` as one project instead of three categories.
- Find a real second copy of a large file. An APFS clone must not be offered as reclaimable space.

Out of scope, on purpose: removing an app that is still installed, similar photos, malware scanning, memory or startup-item cleaning, stripping architectures or localizations, and any deletion while the app is not open.

## 3. What already exists

These behaviors stay. This spec extends them.

| Behavior | Where it lives |
|---|---|
| Allow-list suggestions. Anything unrecognized is not suggested. | `crates/cleanup/src/suggest.rs` |
| Preselected only for old installers, Xcode DerivedData, and old device-support files. | `Category::safety` |
| Review-only: archives, package caches, build folders, app caches, logs, large files. | `PRD.md` section 8.1 |
| Shown, never basketed: Photos, device backups, Mail, Messages, Docker, virtual machines. | `crates/cleanup/src/managed.rs` |
| Refused paths cannot be added, including `~/Library` itself, Preferences, Keychains, Mail, Messages, and `com.apple` containers. | `crates/cleanup/src/safety.rs` |
| Move with `trashItemAtURL`, re-check file identity, skip a running owner. | `crates/cleanup/src/trash.rs` |
| Will free uses private size so clones and hard links are counted once. | `scanner::measure`, tested in `crates/cleanup/tests/will_free.rs` |
| Local operation log: time, category, count, bytes, original paths. No Trash URL and no per-item identity. | `crates/cleanup/src/log.rs` |
| Cleanup History shows that log. Emptying the Trash deletes only what this app moved, using the Trash URL from the current result. | Clean menu, `basket_ui.rs` |

`~/Library/Containers/<id>` is refused as a folder (`names.len() == 2` returns `ManagedByApp`). Files inside a non-Apple container are allowed. Leftovers need the container folder itself, which is the safety change in section 5.3.

`managed_places` is a separate list from `safety::check`. A path can pass the location rules and still be Docker or a virtual machine. Leftovers and exact copies must exclude those roots themselves, not only paths `safety::check` refuses.

Build folders today (`suggest.rs`):

- `node_modules`, `dist`, `.next`, or `.turbo` next to `package.json`
- `Pods` next to a `Podfile`
- `.venv` next to `pyproject.toml` or `requirements.txt`
- `target` next to `Cargo.toml`, and only when `CACHEDIR.TAG` or `.rustc_info.json` is inside `target`
- `.build` next to `Package.swift`
- At least 10 MB
- Not modified in 7 days. "Modified" is the folder plus at most 500 immediate children, not the whole tree.
- Not tracked by Git
- At most 300 folders are checked and 50 items are kept

There is no running-tool check on build folders. `trash::in_use` does not treat `node_modules`, `dist`, `target`, or `.build` as owned by a running command. This version does not add that check.

An item is skipped when it or an ancestor was already claimed (`Rules::taken`). A parent claimed after a child is not rejected. Section 8 requires that both directions hold for the new categories.

## 4. Goals and non-goals

### Goals

- Suggest leftover folders for bundle IDs that this Mac's Applications inventory did not contain, and say that this is a candidate, not a proof the app is gone from the world.
- Let the user put back one item this app trashed, without restoring anything else in the Trash and without overwriting.
- Group eligible build folders under the project directory they belong to.
- Suggest an older exact copy only when its private size is worth freeing, and only while the kept copy is still the same file.
- Keep every new item review-only.
- Keep the safety tests green, and add tests for each new allowance and refusal.

### Non-goals

- Uninstalling an app that is still on disk, including its `/Applications` bundle.
- Searching external volumes or other home folders for apps. Absence from this Mac's Applications folders is not "uninstalled".
- Matching leftovers by display name ("Slack", "Docker"). Names collide. This version matches bundle IDs only.
- Group Containers. Apple group identifiers are names, and more than one app can share a container. A folder named `group.<id>` or `<team>.<id>` stays refused.
- LaunchAgents. A file name does not prove the bundle id inside the plist, and a loaded helper can keep running after the app bundle is gone.
- Cleaning `~/Library/Preferences`, Keychains, Cookies, Accounts, or Sharing. Those folders stay refused.
- Cleaning `com.apple.*`, MobileSync backups, Mail, Messages, Photos, or iCloud Drive.
- Similar or near-duplicate photos.
- Hashing on the UI thread, or hashing dataless files (that would download them).
- Background cleanup, a menu-bar helper, or scheduled deletion.
- Malware, RAM, login-item "optimization", architecture stripping, or localization pruning.
- A new "tool is not running" rule for build folders. That would be a behavior change, not a regrouping.

## 5. Leftovers of apps that were not found

### 5.1 Applications inventory

Before suggesting leftovers, read `CFBundleIdentifier` from `Contents/Info.plist` of every `.app` under:

- `/Applications`
- `/System/Applications`
- `~/Applications`, when that directory exists

Walk directories at any depth so `/Applications/Vendor/Editor.app` is found. Do not walk the inside of an `.app`, except the single `Contents/Info.plist` read. A symlink that resolves to an `.app` counts as that app. A symlink named `.app` that does not resolve is an unknown app. Do not use Spotlight. Do not look on external volumes.

`~/Applications` missing is normal. Skip that root and continue.

If `/Applications` or `/System/Applications` cannot be read, omit the leftovers category and say the app list could not be read. A partial list is not used.

An `.app` whose plist is missing, unreadable, or has no bundle id is an unknown app. If any unknown app was found, omit leftovers and say how many apps could not be identified.

`RunningApps::current` omits processes whose paths it cannot read. Those processes are unknown. If any process path could not be read, omit leftovers. A running app whose bundle was found is installed. A running app whose bundle was not found is still treated as installed when its bundle id is known from the process. An unknown running process blocks the category rather than being ignored.

The inventory has a generation: the set of bundle ids, plus whether it was complete. A later walk replaces it.

The card must not say "this app was removed". It says "Not found under Applications on this Mac" and lists the roots that were read.

### 5.2 What can be suggested

A leftover is one of these, only when the inventory in section 5.1 completed and the bundle id is absent from it:

| Location under `~/Library` | Match | Row note |
|---|---|---|
| `Caches/<bundle-id>` | Folder name equals the bundle ID | The app recreates this |
| `Application Support/<bundle-id>` | Folder name equals the bundle ID, and is not `MobileSync` | May contain data the app saved |
| `Containers/<bundle-id>` | Folder name equals the bundle ID | May contain data the app saved |
| `Saved Application State/<bundle-id>.savedState` | Exact file name | May contain data the app saved |
| `HTTPStorages/<bundle-id>` | Folder name equals the bundle ID | May contain data the app saved |

`<bundle-id>` is a reverse-DNS identifier (`com.example.app`). A folder named with spaces or a single word is not a match.

Always skip:

- `com.apple.*`
- Group Containers and LaunchAgents
- Preferences, Keychains, Cookies, Accounts, Sharing
- MobileSync, Mail, Messages, Photos, iCloud Drive
- A path `safety::check` refuses, other than the container-folder case in section 5.3
- Dataless items
- Symlinks that resolve outside the scanned folder
- Anything outside the scanned folder
- A managed place from `managed.rs`, its descendants, and any folder whose removal would include one. A container that holds `Docker.raw` or a virtual machine is not a leftover.

Size: folders under 1 MB are omitted.

The suggestion is one card, "Leftover files not tied to an installed app". Items are largest first. Each row shows the bundle ID, the path, the size, the note from the table, and "Not found under Applications on this Mac". The card is never preselected. Adding it still goes through the basket and the confirm step.

One category owns a path. Leftovers do not take a path already claimed by app caches, package caches, logs, or large files. If a child is already claimed, the parent folder is not claimed either.

### 5.3 Container allowance

Today `~/Library/Containers/<id>` is refused as a folder. That refusal stays for every container that section 5.2 did not match, including every `com.apple` container and every container whose app is installed or unknown.

`safety::check` stays a path check. It does not learn a leftover list. Basket add and the Trash move both call it again, so a suggestion-only exception cannot be trashed, and a blanket exception would be too wide.

The allowance is a proof value passed into add and into confirm, not a hole in `check_path`:

- bundle id
- canonical path of the container folder itself, not a file inside it
- file identity (device, inode, directory, not a symlink)
- inventory generation from section 5.1
- purpose: leftover container

Add and confirm recompute the proof. It is invalid, and the add or move is refused, when:

- the inventory generation has changed, or the new inventory is incomplete or unknown
- the bundle id is now installed or running
- the identity changed
- the path is not that container folder
- the folder contains managed data
- the scan was replaced or the scope changed

Contents of a container are not offered one by one. The user trashes the container folder or nothing.

`~/Library/Preferences` stays refused, including `<bundle-id>.plist`.

Group Containers stay refused. There is no substring test to implement, because the row is not offered.

Tests must show:

- A matched container of a third-party app absent from a complete inventory can be added, and can be moved, only while the proof still holds.
- The same container is refused when that bundle id is installed, when the inventory recorded an unknown app, or when the proof is stale.
- `com.apple` containers, Group Containers, Preferences, Keychains, and MobileSync stay refused.
- A container that contains a managed file is refused.
- Suggesting the container does not by itself make `safety::check` return ok.

## 6. Put back

`trashItemAtURL` returns the Trash URL. `Moved.trashed` keeps it for Empty Trash on the current result. `OperationLog` stores one aggregated row per category and drops the URL and the identity. Put back needs the URL in the log. It does not replace the current-session Empty Trash path.

### 6.1 Log

New item records, in addition to the existing category summary. Old lines without item records still deserialize. They do not offer Put back.

Each moved item records:

- a stable item id
- original path, as filesystem bytes, not a lossy display string
- Trash path, as filesystem bytes
- volume identity of the original parent
- file identity at the Trash location after the move
- category, size, time
- state: `trashed`, `restored`, `deleted`, or `restore-failed`, plus the last failure reason

The log stays in the app's support folder. Paths in it are local only and are not sent to Sentry.

If the log append fails after a move, the result screen says Put back is unavailable for this cleanup because the history could not be saved. The current Empty Trash action can still use `Moved.trashed` for that session.

### 6.2 Action

Cleanup History shows Put back on an item whose state is `trashed` and whose record has a Trash path. Restored and deleted items have no Put back. One confirmation can cover every `trashed` item in an entry. Each item succeeds or fails on its own. A failed item can be tried again. A restored item is not tried again.

Put back does not call `safety::check` on the destination. That returns `Missing` when the path is free, which is the success case. It does not call `safety::check` on the Trash path. That returns `InTrash`.

Source check:

- the recorded path is inside a Trash directory
- the item is there
- the file identity matches the log

Destination check:

- the parent exists and is the original parent
- the volume identity matches the record, so a different disk mounted at the same path is refused
- the original path does not exist

The move itself must refuse an occupied destination. Checking and then renaming is not enough. `FileManager.moveItem` refuses an existing destination and is the primitive to use.

A failure reason is one of: gone from the Trash, identity changed, destination occupied, parent missing, volume changed, permission, no recovery record. The row shows that reason.

A successful put back sets the item to `restored` and appends a `Restored` action. It does not delete the history of the move.

Empty Trash deletes only items this app moved that are still `trashed`. It uses the Trash path from the item record, not a path from a previous attempt. A restored item is not deleted. The deletion result is stored per item. A category summary must not report `failed: 0` when an item deletion failed.

## 7. One card per project

`.next`, `.turbo`, `Pods`, and `.venv` are eligible under the same size, age, and Git gates as the folders in section 3. There is still no running-tool check.

Group those items by the directory that holds the manifest (`package.json`, `Cargo.toml`, `Package.swift`, `Podfile`, `pyproject.toml`, or `requirements.txt`). The card's identity is that directory's full path. The title is the folder name. When two cards would show the same name, the title includes the parent folder.

- Reason: "Dependencies and build output in this project, untouched for 7 days at the top of the folder and not in Git. The next build recreates them."
- The reason does not say every file inside was untouched.
- Rows: each folder, with its size
- The card is review-only

Xcode DerivedData stays its own card. Device support and archives are unchanged.

Two projects are never merged. The 300-check and 50-item caps stay. When a cap is hit, the suggestions area says the list was cut.

A folder that fails age, size, or Git is left off the card. Git and "could not check Git" stay the skip lines suggestions already produce. Age and size do not get new per-project essays. There is no "tool is running" line, because the code does not check that.

Cargo `target` is still only a build folder when `CACHEDIR.TAG` or `.rustc_info.json` is present.

## 8. Exact copies that would free space

After a scan finishes, a background task may look for duplicate files. It must not run on the UI thread. Cancelling the scan, changing scope, a rescan, an FSEvents replace, or a cleanup drops the result, including any node ids. The user can press Stop. Stop drops a hash that was in progress and does not publish it.

### 8.1 Candidates

- Regular files only, opened without following symlinks
- Inside the current scan root
- Logical size at least 50 MB
- At most 200 candidates, largest first
- Also stop after 20 GB of bytes read or 2 minutes, whichever comes first
- Skip dataless files. Do not read them.
- Skip anything inside a package (`.app`, `.photoslibrary`, and the rest of the package list in `safety.rs`), inside `.git`, on a refused path, or on a managed path or under a managed ancestor
- Skip a file whose ancestor is already claimed by another suggestion

Group by size. A size with one file is done. For a size with two or more files, record device, inode, size, and modification time, hash with SHA-256, then read that metadata again. If it changed, drop the file. Do not publish a partial hash.

### 8.2 What is suggested

For each hash with two or more files that still match their recorded metadata, choose the kept file: the newest modification time. If the times are equal, keep the path that sorts first as bytes, and say "Same date. This path is kept."

Measure private size with `scanner::measure` on the removal set only, not on the kept file. Hard-link bytes count only when every link is in the measured set, which is the rule `measure` already uses. Suggest a copy only when removing that one file would free at least 50 MB. The basket's Will free for that row must use the same measurement. An unedited APFS clone is not suggested.

Before the move, recheck both the copy and the kept file. Refuse the move when:

- either file's identity or content metadata changed after the hash
- the kept file is missing
- the kept file is in the basket, or inside a folder in the basket
- the copy is inside a folder already being removed

One review-only card per file type, titled `Exact copies · Videos` and so on. A type with no copies produces no card. While the search is running, and when it finishes with nothing to remove, there is still a single Exact copies card for the progress line or the empty result. Each row shows both paths, the private bytes that would be freed, and "Identical contents. The newer file is kept." or the same-date line.

The card shows progress while hashing. It says when it stopped because of the file cap, the byte cap, the time cap, or Stop. "Nothing to remove" is only for a finished search that found nothing. A capped search says it was incomplete.

Never preselect. Never suggest deleting every copy.

## 9. User experience

The suggestions strip and the basket are unchanged in structure.

- New cards use the same "Review first" treatment as build folders. There is no new "Safe to delete" category.
- Leftovers show "Looking…" until the inventory finishes, then the items, "Nothing to remove", or the reason the category was omitted (app list unreadable, unknown apps, unknown processes).
- Exact copies show progress, Stop, and a capped or incomplete state.
- Put back lives in Cleanup History, not in the basket. Each item shows its state and, on failure, the reason from section 6.2.
- A refused leftover uses the existing refusal text when `safety::check` refused it. A stale proof says the app list changed and the item must be reviewed again.

## 10. Acceptance

Fixtures are temporary directories. Tests that mount a disk or scan the home folder stay `#[ignore]`.

| Check | Pass |
|---|---|
| Nested app | `/Applications/Vendor/Editor.app` counts as installed. Its cache is not suggested. |
| Missing third-party bundle | Its cache, support folder, and container are one review-only card. Preferences, Group Containers, and LaunchAgents are absent. The row does not say the app was removed. |
| Same bundle installed | Those folders are not suggested. The container stays refused without a fresh proof. |
| Unknown plist or unreadable process path | No leftovers card, with a reason. |
| `~/Applications` missing | Leftovers still run when the other roots were read. |
| `/Applications` unreadable | No leftovers card. The status says the app list could not be read. |
| `com.apple` and MobileSync | Not suggested. Adding them still fails. |
| Managed file inside a container | The container is not suggested and cannot be added. |
| Mail and Photos | Mail Downloads and a Photos library are shown and open their app. Neither can be added to the basket. |
| Stale proof | A reinstall, a new inventory generation, or a changed identity refuses the move. |
| History put back | An item this app trashed returns to its original path. An occupied destination is left alone. An old log row without an item record has no Put back button. A restored item is not deleted by Empty Trash. |
| Partial restore | One failure does not cancel the items that succeeded, and the failed item can be tried again. |
| Log append failed | The result says Put back is unavailable. It does not offer a button that cannot find the Trash URL. |
| Project card | `node_modules`, `dist`, and `.next` of one project are one card. `.turbo`, `Pods`, and `.venv` join the project that holds their manifest. A Cargo `target` without `CACHEDIR.TAG` or `.rustc_info.json` is not on a card. Two projects with the same folder name show a parent in the title. |
| Screenshots | A screenshot on the Desktop untouched for 30 days, at least 200 KB, is one review-only row. A screenshot from today is not. The same name inside a Photos library is not. A custom Screen Capture folder is used when that setting is present. |
| Clone | Two clones of a 100 MB file produce no duplicate row. |
| Real copy | Two independent 100 MB files with the same bytes produce one row, the older file, and Will free for that row is at least 50 MB. Finished copies are one card per file type. |
| Copy changed after hash | The move is refused. |
| Kept file in the basket | The duplicate move is refused. |
| Dataless file | It is not read and not suggested. |
| UI thread | Hashing and the applications walk do not run on the UI thread. Stop drops an in-progress hash. |

Existing tests still pass: refused paths cannot be added, and Will free on the throwaway volume stays within 1% of the free-space change.

## 11. Order of work

1. Leftovers, including the container proof and its tests. No Group Containers. No LaunchAgents.
2. Put back, including per-item log records. Old rows keep loading.
3. Project cards. Presentation only, using the eligibility rules in section 3.
4. Exact copies, after sections 5 and 8 are the rules the code follows. Do not start hashing before the proof, the kept-file check, and the managed-file exclusion are in the implementation plan.

Each of the first three steps is shippable alone. Exact copies are last.

## 12. Risks

- **A finished inventory can still miss an app** that lives only on an external disk or in a folder we do not walk. The copy says "not found under Applications on this Mac" and the category is omitted when the inventory is incomplete.
- **The container allowance is a real safety change.** It is a proof checked at add and at confirm, not a general opening of `~/Library/Containers`.
- **Put back can fail after the user empties the Trash.** The item state becomes `deleted` or the move fails with "gone from the Trash".
- **A log failure after a successful move** is the case where recovery cannot be offered. The screen has to say so.
- **Hashing** can still read 20 GB. The caps and Stop are the bound. The card must say when the search did not finish.
- **Private size** must use `scanner::measure` on the files that would actually be removed. Measuring the kept file together with the copy would hide a clone or invent free space.
