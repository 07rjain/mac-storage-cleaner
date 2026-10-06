# AGENTS.md

## Working principles

- When explaining something to the user, use the `visualize` skill.
- Be concise, direct, and candid. Challenge weak assumptions and distinguish verified facts from uncertainty.
- Ground research in authoritative, current sources and link important evidence.
- Preserve the original goal and constraints; finish authorized work end to end and verify the actual result before claiming completion.
- Ask questions only when a decision is materially ambiguous, risky, or requires approval.
- Use relevant skills; spawn subagents only for genuinely independent work and synthesize their findings.
- Keep changes focused and simple. Avoid unrelated edits, unnecessary abstractions, and low-signal tests.
- Test observable behavior, review substantial changes, and validate user-facing work in the real interface when applicable.
- Preserve unrelated work and never take destructive, production, or external actions beyond what the user authorized.
- Report meaningful blockers, outcomes, and evidence without noisy progress.

## Project map

Native macOS storage visualizer and cleaner (Apple silicon, macOS 14+). Rust workspace, GPUI pinned in the root `Cargo.toml`.

| Path | Role |
|---|---|
| `crates/app` | Window, sunburst, treemap, icicle, tree, file types, basket |
| `crates/scanner` | Read-only `getattrlistbulk` walk, arena `Tree`, FSEvents watch |
| `crates/cleanup` | Suggestions, basket, Trash, operation log |
| `crates/volumes` | Capacity, purgeable space, snapshots |
| `crates/telemetry` | Sentry setup, path scrubbing, native crash dumps |
| `crates/scan-cli` | `msc-scan` accuracy and benchmark harness |
| `PRD.md` | Product requirements. Update it when behavior or measurements change |
| `CLEANUP_PRD.md` | Next cleanup: leftovers, put back, project cards, exact copies |
| `scripts/bundle.sh` | Release app bundle, ad-hoc signature, DMG, dSYM |
| `scripts/upload-dsym.sh` | Sentry release and dSYM upload |

iCloud features, Developer ID signing, and notarization are deferred. Do not add them unless the user asks.

## Development commands

Toolchain is `rust-toolchain.toml` (Rust 1.99, `aarch64-apple-darwin`).

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Opt-in tests that scan the home folder or mount a disk image are `#[ignore]`. Do not run them against the user's real disk unless they ask.

## Changelog

`CHANGELOG.md` follows Keep a Changelog and Semantic Versioning. The app version is `workspace.package.version` in `Cargo.toml`.

- User-visible changes, dependency pins, Sentry setup, and safety-rule changes get a line under `## [Unreleased]` in the same change.
- A release moves `Unreleased` into `## [x.y.z] - YYYY-MM-DD`. Use that same version for the bundle, the git tag `vX.Y.Z`, and the Sentry release name `mac-storage-cleaner@X.Y.Z`.
- Do not tag or publish a release unless the user asks.

## Error handling and Sentry

- The app reports with the DSN baked in at build time from `SENTRY_DSN` (environment or `.env`). No DSN means crash reporting is off and the app still runs.
- `.env` is gitignored. `SENTRY_AUTH_TOKEN` (also stored as `sentry_api`) is a user auth token for `sentry-cli` only. Never compile it into the app, commit it, or print it. `scripts/upload-dsym.sh` passes it through the environment.
- `send_default_pii` is off. `before_send` / `before_breadcrumb` in `crates/telemetry` drop or scrub paths, file names, volume names, and user names. If an event cannot be scrubbed, it is not sent.
- Rust panics use the Sentry panic integration. Native crashes (`SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, and `SIGABRT` that is not a Rust panic) are written by the signal handler in `crates/telemetry/src/native.rs` and sent on the next launch. The dump is instruction addresses, image load addresses, sizes, UUIDs, and library file names only. No paths, user names, or memory.
- Crash reports are on by default and must stay a runtime opt-out (`Settings.crash_reports`). Turning them off takes effect immediately.
- Do not `expect` or panic on user disk data. `Tree::replace` returns `false` when it cannot apply a rescan. Live refresh must not keep appending copies of the tree; replacing the scan root swaps the tree, and removed nodes are reclaimed. A replace changes node ids, so the UI rebinds the open folder and selection by path.

## DMG and install

- `./scripts/bundle.sh` writes `dist/Mac-Storage-Cleaner-<version>.dmg` and `dist/mac-storage-cleaner.dSYM`. `dist/` is gitignored. The script signs ad hoc with the hardened runtime. It is not notarized, so first launch needs System Settings › Privacy & Security › Open Anyway.
- Check for Updates reads `https://api.github.com/repos/07rjain/mac-storage-cleaner/releases/latest` with no token. It only opens `https://github.com/07rjain/mac-storage-cleaner/` URLs. A private repository returns 404, so the repo has to be public before the button can see a release. Do not embed a GitHub token to work around that.
- Ad-hoc signatures change every build. Full Disk Access must be granted again after updating.
- `./scripts/upload-dsym.sh` creates the Sentry release and uploads the dSYM from `dist/`. Run it only when the user asks to publish symbols. It needs `SENTRY_AUTH_TOKEN`, `SENTRY_ORG`, and `SENTRY_PROJECT`.
- A GitHub release attaches `dist/Mac-Storage-Cleaner-<version>.dmg` to tag `vX.Y.Z`. The README links to the latest release. Do not commit the DMG.
- To try a build locally: `hdiutil attach` the DMG, copy `Mac Storage Cleaner.app` to `/Applications`, `xattr -cr` the copy, then `open` it. Quit the running app first.

## Safety and scope

- The scanner is read-only and must not download iCloud files. Do not call `brctl`, `fileproviderctl`, or evict/download APIs.
- Cleanup moves files to the Trash. Do not unlink user files. Refused paths stay refused.
- Do not commit `.env`, tokens, or crash dumps. Do not force-push or skip git hooks unless the user asks.
