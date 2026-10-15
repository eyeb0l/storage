# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

A local, read-only macOS storage analyzer. `scan.py` walks the home folder and writes `report.html`: a treemap explorer plus cleanup recommendations that build a bash script the user runs themselves. Nothing in this repo ever deletes files.

## Commands

- `python3 scan.py` — scan `~` (~40 s), write `report.html`, open it in the browser
- `python3 scan.py --no-open` — same, without opening
- Python 3 stdlib only; no dependencies, no build step, no tests.

## Architecture

- **scan.py** — single-pass `os.scandir` walk of `$HOME` (sizes are `st_blocks * 512`, hard links deduped, other volumes skipped). During the walk it:
  - builds the treemap tree, pruned to `MIN_NODE`/`MAX_CHILDREN`/`MAX_TREE_DEPTH`, with leftovers merged into an `"o"` (aggregate) node. Node keys: `n` name, `s` bytes, `t` category, `c` children, `l` display label, `f` file, `u` unreadable.
  - matches cleanup candidates ("entries": `r` rec id, `p` home-relative path, `s` bytes, optional `label`, `cmd`, `d` idle days, `m` mtime) against rule tables, in `match_dir` priority order: `SKIP` → `FIXED` → `CHILDREN_OF` → `GRANDCHILDREN_OF` → path patterns → project patterns (`pat_ok`: only outside `Library`, `Applications` and top-level dot folders). Once a folder matches, nothing beneath it can match again (`claimed`), so entries never overlap.
  - `RECS` defines each recommendation's title, tier (`safe` = regenerated automatically, `review` = user decides) and copy. `manual` recs are shown but never put in the script.
- iCloud/File Provider folders (`CLOUD_ROOTS`) can hang indefinitely when listed. They are measured per child with `du` under a timeout, never walked.
- Ollama models are read from manifests before the walk so blob files get model-name labels and `ollama rm` commands.
- **template.html** — the whole UI (vanilla JS/CSS, no external resources). `scan.py` replaces the literal `/*__DATA__*/null` with the JSON payload. Squarified treemap renders two levels (`unwrap` skips single-child chains). Selection is persisted in `localStorage`. `buildScript()` emits `del` calls guarded to `$HOME` plus each entry's `cmd`, behind a `/dev/tty` y/N prompt.
- `report.html` is generated output; edit `template.html` instead.

## Verifying UI changes

No browser automation is set up here. JS syntax can be checked with `/System/Library/Frameworks/JavaScriptCore.framework/Versions/Current/Helpers/jsc -e "checkSyntax('file.js')"`. For screenshots, render with a small WKWebView Swift script (`swiftc`) and take a snapshot.
