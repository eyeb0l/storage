# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Storage is a macOS app (Rust + Tauri v2) for developers who work with coding agents. It scans the home folder and the dev-relevant folders outside it, sorts every byte into a category for a dot-matrix overview, and finds what can be cleared, with special care for the git worktrees that agents (T3 Code, Claude Code, Codex, Conductor, Cursor) create per task.

## Commands

The Rust toolchain lives in `~/.cargo/bin`; add it to `PATH` if `cargo` isn't found.

- `cargo run -p storage-app`: run the app (debug build; the UI in `ui/` is embedded at compile time, so rebuild after UI edits)
- `cargo run --release -p storage-core --example scan [-- --json]`: read-only scan from the terminal (about 9 s)
- `cargo test -p storage-core`: unit tests plus end-to-end tests against a fake home folder in a temp dir (they create real git repos and worktrees, and use `Scanner::home_only()` so nothing outside the fake home is read)
- `cargo clippy --workspace --all-targets` and `cargo fmt --all` (120 columns, see `rustfmt.toml`)
- `npx @tauri-apps/cli@2 build --bundles app`: release `.app` in `target/release/bundle/macos/` (or `cargo install tauri-cli` and `cargo tauri build`)

## Architecture

- **crates/storage-core**: everything that touches the disk. No Tauri dependency.
  - `scan.rs`: one parallel `rayon` walk of `$HOME` plus `rules::SYSTEM_ROOTS` (/Applications, /Library/Developer, Homebrew, /tmp, …) and the per-user temp and cache folders under /var/folders (16 threads). Paths are home-relative inside home and absolute outside it; outside roots are `quiet` (root-owned folders aren't reported as unreadable). Simulator runtimes can't be walked, so `sim_runtimes` asks `simctl runtime list -j` (only when a full Xcode is selected, to avoid the install prompt). Sizes are `st_blocks * 512`, hard links are counted once, other volumes and symlinks are skipped. Every file adds to exactly one `Cat` (the test asserts `scanned == sum(cats)`). Folders matching a rule become `Entry`s, in `match_dir` priority order: `SKIP` → `FIXED` → `CHILDREN_OF` → `GRANDCHILDREN_OF` → path patterns → project patterns (`pat_ok`: only outside `Library`, `Applications` and top-level dot folders, or anywhere inside a worktree). Once a folder matches, nothing beneath it can match (`claimed`). Temp-folder rules set `min_age`, so anything touched in the last day is skipped. The exception is a linked git worktree (a `.git` file pointing into `…/.git/worktrees/`): it's an entry, its contents count as `Cat::Worktrees`, and its build output, `node_modules` and venvs are entries too, with `parent` set to the worktree's key.
    The walk also returns a pruned `Node` tree for the drill-down view (`MIN_NODE`, `MAX_CHILDREN`, `MAX_TREE_DEPTH`; leftovers fold into an `o` aggregate). Its root, "This Mac", holds one node per scan root, the simulator runtimes and a virtual (`x`) node for the part of the disk that wasn't scanned. Node keys are short (`n`, `s`, `t`, `c`, `p`, `f`, `o`, `u`, `x`) because the tree is sent to the UI whole.
  - `rules.rs`: categories, recommendations (`RECS`) and the path tables. `Tier::Safe` means regenerated automatically, `Tier::Review` means the user decides. `child_cat` decides a folder's category: worktree > matched rec > `CAT_PATHS` > parent.
  - `git.rs`: worktree detection and a read-only inspection (`GIT_OPTIONAL_LOCKS=0`): branch, uncommitted files, commits only on a detached HEAD, merged into the default branch, upstream pushed or `[gone]` (squash-merged PRs), orphaned, last activity.
  - `clean.rs`: acting on entries. Safe → deleted (`force_remove` handles read-only folders); Review → moved to the Trash via NSFileManager, then `git worktree prune` for worktrees; entries with `cmd` (simulators, Ollama, Docker) run that command; the Trash rec empties `~/.Trash`. Every path goes through `guard`: inside home and not a protected top-level folder, or outside home only in /tmp, the user's /var/folders temp and cache folders, or a whole app in /Applications, and owned by the user; never a symlink.
  - iCloud/File Provider folders (`CLOUD_ROOTS`) can hang when listed, so each child is measured with `du` under a timeout.
- **src-tauri**: thin shell. Commands: `scan` (streams `Progress` over a `Channel`), `clean` (takes entry *keys*, never paths; looks them up in the stored report), `disk`, `reveal` (an entry), `reveal_path` (a tree node; only reveals in Finder), `open_privacy_settings`. One scan or cleanup at a time (`Busy`).
- **ui/**: vanilla JS/CSS, no build step, no external resources (strict CSP). `DotBar` draws the canvas: dot-matrix segments per category, hover highlight, scan edge and the particle dissolve when something is cleared. The model in `S` is updated optimistically from clean events; Docker sizes are left alone because its disk image doesn't shrink right away. The Explore tab is a squarified treemap of the scan tree (two levels at a time, Escape to go up); tiles that are entries get a checkbox that shares the selection, and `treeAdjust` shrinks the tree as things are cleared. Suggestions are grouped by recommendation category and skip anything used in the last three days or inside a worktree active in the last two hours.

## Verifying UI changes

No browser automation is set up. Check JS syntax with `/System/Library/Frameworks/JavaScriptCore.framework/Versions/Current/Helpers/jsc -e "checkSyntax('ui/app.js')"`. For screenshots, either run the app and capture its window (`screencapture -l <window id>`), or load `ui/` in a WKWebView from a small Swift script with a mocked `window.__TAURI__` that returns the JSON from the `scan` example. An off-screen WKWebView throttles `requestAnimationFrame`, so give the window an on-screen frame (alpha can be near zero).
