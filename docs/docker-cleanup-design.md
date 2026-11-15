# Itemised Docker cleanup investigation

Status: read-only panel and review-token flow implemented after approval, 15 November 2026.
**Do not enable removal.** No real Docker executable or engine was used for validation. The
existing Docker cleanup refusal, Docker Desktop context and filesystem usage reporting remain intact.

## What the application currently does

- `scan.rs::match_file` recognises one fixed `Docker.raw` path. Its size is allocated filesystem
  bytes, counted once in `Cat::Containers`; the resulting `Entry` has a key, path, command and hold.
  It has no Docker object IDs or per-object state.
- `ui/app.js::confirmClear` confirms selected entries by name, size and owning-tool command. A hold
  excludes an entry from automatic suggestions, but does not prohibit manual selection. Cached
  scan results are inert, and a completed new scan clears the selection.
- `src-tauri/src/main.rs::clean` resolves selected keys against `AppState.report`, takes `Busy`,
  calls `clean_noting` sequentially, logs outcomes and updates the report. It never accepts paths
  or arbitrary commands from the webview. `Busy` serialises Storage operations, not Docker clients.
- `clean_noting` refuses `rec == "docker"` before selecting a method. This is the enforceable
  safety boundary; neither a missing command nor clearing the UI hold bypasses it.
- `rules.rs::command` still describes the old `system prune -a -f`, explicitly pinned to
  `desktop-linux`. It is not an itemised cleanup plan and must never be reused as a fallback.
  The existing runner removes inherited `DOCKER_HOST` and `DOCKER_CONTEXT` overrides.

## Inventory and interface model

Keep filesystem usage and Docker object inventory separate. Do not add Docker object sizes to
`Report.scanned`, category totals or the filesystem tree: they describe data already inside
`Docker.raw`, use different accounting and share layers. A separate Docker detail panel should
show inventory availability, collection time, engine/context identity and individual rows.

| Kind | Identity and details to show | Size meaning | Initial removal policy |
| --- | --- | --- | --- |
| Container | Full container ID; name; image ID/reference; Compose project/service; status; creation/start/finish times; mounts; writable-data warning | `SizeRw` is writable-layer bytes; logs, mounted data and host recovery are separate | Block running, paused, restarting and removing states. Keep stopped containers blocked until mutable-data race is resolved. |
| Image | Full content ID; every tag and digest; platform/manifest details; references from **all** containers, including stopped ones | Docker logical image size includes shared layers; unique/shared bytes are estimates requiring raw Engine disk usage | Initially block referenced images and aliases. Even an untagged image needs race and backend compatibility proof before enabling deletion. |
| Build cache | Engine/builder/worker-scoped record ID; type; description; parents; immutable/mutable; in-use; shared/private; creation/last-use time and usage count | Shared records may release metadata only; do not sum cache and image sizes as recovered space | Block mutable, in-use, shared, internal/frontend, unknown-scope or incompletely inventoried records. |

Each row needs a specific unavailability reason and a full-ID detail view. Display missing byte
counts as unknown, never zero. Group identical image IDs across tags into one row. Escape all
Docker-provided labels in HTML. Retain full inspect data in Rust, but expose only allowlisted
display fields: inspect records may contain environment values, credentials or host paths.

Volumes, networks, builder instances and `Docker.raw` itself are outside the removal scope. A
container confirmation must explain that its writable data and metadata would be lost permanently,
while mounted volumes and bind-mount contents stay. Docker's `rm --volumes` is a separate destructive
option and must not be used. [Container removal reference](https://docs.docker.com/reference/cli/docker/container/rm/)

The final confirmation must list **every selected target**, its exact identity and references,
its irreversible effects and its current eligibility. “Docker: up to N GB” is insufficient.
Do not preselect Docker objects or fold them into “clear everything safe”. Show host free-space
change only from the post-operation disk reading; logical bytes removed are a separate metric.
Docker Desktop stores its engine data in a disk image, whose allocation/recovery differs from
individual object accounting. [Docker Desktop for Mac FAQ](https://docs.docker.com/desktop/troubleshoot-and-support/faqs/macfaqs/)

## Proposed confirmation and removal protocol

1. **Collect read-only inventory.** Resolve the explicit `desktop-linux` context from the expected
   home configuration. Reject remote endpoints and unknown socket layouts. Record the validated
   endpoint, engine ID, API/backend/version capabilities and, for cache, builder and worker IDs.
   Clear inherited `DOCKER_*`, `BUILDX_*` and `BUILDKIT_*` overrides. Failure, timeout, malformed
   output, missing permission or partial pagination means unavailable, not an empty inventory.
2. **Store the snapshot in Rust.** Give it a generation ID and an opaque backend-issued review
   token. The UI sends snapshot ID plus selected keys. Unknown/duplicate keys, expired generations
   and unavailable objects refuse the whole plan. Never accept user-supplied IDs, CLI arguments,
   endpoints or inspect bodies as removal authority. Keep this separate from filesystem `Entry`:
   there must be no route to filesystem deletion or the old stored command.
3. **Prepare a frozen review.** Resolve only those keys, calculate all side effects and return an
   allowlisted confirmation view. Store the selected identities and reviewed metadata privately.
   Rescanning, restarting Storage or changing the endpoint invalidates it. No automatic expansion
   to new objects, parents, aliases, related containers or newly eligible resources is permitted.
4. **Revalidate after confirmation.** Take `Busy`, consume the one-use token and compare the full
   current inventory against the reviewed subset, including image users and cache worker identity.
   Refuse on disappearance, replacement, new references, tag/state/mount/configuration changes,
   restart history, changed last-use data or an incomplete response. A new unrelated object is
   left alone; it is never appended to the plan. Repeat the checks immediately before each item.
5. **Execute only a proven bounded operation.** Pin a connection to the validated local engine;
   repeatedly loading a mutable named context is not sufficient for destructive operations.
   Use complete IDs, one target per request, no shell, no name/tag/prefix selectors and no force,
   volume, recursive-parent or global-prune fallback. Failures must stop the batch. Changes caused
   by earlier selected removals require explicit plan semantics or another review, not silently
   relaxing later comparisons.
6. **Reconcile outcomes.** Verify the specific target disappeared and audit daemon-reported effects
   against the frozen plan. Stop on unexpected effects. A timeout or lost connection is an unknown
   result, not proof that no deletion occurred; reconcile before offering a fresh review. Stop
   requests prevent the next item; completed removals cannot be rolled back. Refresh Docker usage
   and system free space; never subtract logical object sizes from `Docker.raw` optimistically.

Steps 4 and 6 detect errors but cannot undo an over-broad deletion. They do not substitute for
step 5's server-side guarantees. A review token protects Storage's own request flow; it does not
lock Docker Desktop, another CLI client, a build, scheduled GC or the daemon's state.

## Removal primitives and the unresolved race

**Containers:** a full-ID `container rm` without force or volume removal bounds the container
identity and lets the daemon reject a running container. It does not compare the writable layer,
mount metadata or restart history with the confirmed snapshot. For example, a stopped container
can be written through `docker cp`, or start and stop after the final inspection. Inspecting again
does not close that interval. Strict state matching needs an enforceable mutation barrier or
server-side conditional removal; an application mutex or waiting for Docker to appear idle is
insufficient. This is a design inference from the available inspect/remove API, not a live test.
[Engine API](https://docs.docker.com/reference/api/engine/version/v1.51/)

**Images:** the candidate operation is full-content-ID `image rm --no-prune`, without force. The
no-prune flag matters: otherwise unreviewed untagged parents can be removed. Names and tags can
be reassigned, and removing a single-tag image also removes the tag. A new reference can arrive
between inspection and removal; daemon conflicts must never trigger a forced retry. Full IDs
bound image content, but do not prove the set of aliases/platform effects still matches review.
Containerd image-store and classic-store behaviour both need isolated contract tests before
shipping even a narrow image-removal path. [Image removal reference](https://docs.docker.com/reference/cli/docker/image/rm/)

**Cache:** raw Engine `/system/df` can describe the engine's cache. It is not an inventory of
arbitrary Buildx builders. Buildx `du` inventories the selected builder, and shared cache can
release only metadata; require an explicitly validated single local worker before considering
that transport. Never bootstrap, create or choose a builder from inherited environment settings.
[Buildx disk usage](https://docs.docker.com/reference/cli/docker/buildx/du/)

Buildx supports an ID filter, but current upstream CLI source translates `id=<value>` into a
regular-expression matcher. A raw ID is therefore not an exact-identity guarantee. A prospective
adapter would need an escaped, anchored pattern for one reviewed ID, proven against its pinned
CLI/BuildKit version, plus complete worker scoping and side-effect tests. Do not use `parents`,
age/description filters or size targets as an approximation of selection. Filtering and internal
in-use locks still do not compare reviewed usage metadata atomically. Cache removal stays blocked.
[Buildx prune reference](https://docs.docker.com/reference/cli/docker/buildx/prune/),
[CLI filter translation](https://github.com/docker/buildx/blob/master/commands/prune.go),
[BuildKit filtering and locks](https://github.com/moby/buildkit/blob/master/cache/manager.go)

Under the strict requirement that changed/unreviewed state must never be removed, the documented
stock CLI/API does not establish a complete solution for all three kinds. Retain the safety hold
unless a restricted operation's guarantees are proven, or a daemon-side conditional operation /
exclusive mutation mechanism is available. Do not quietly weaken this requirement to “same ID”.

## Read-only implementation

The Docker tab loads inventory only on request. It shows separate container and image groups,
full IDs in expandable details, state, references, creation time, container image/Compose project
and mount destinations. Docker-reported sizes are labelled as logical image size or writable-layer
bytes; missing sizes are unknown. Shared data is not added to filesystem totals. Build cache has
an explicit unavailable state. Raw inspect environment, mount host paths and configuration never
reach the webview. The Docker selection is memory-only and independent of filesystem cleanup.

`docker_inventory` stores a fresh backend snapshot with a random 256-bit ID and ten-minute
monotonic lifetime. `docker_prepare_review` resolves only keys in that snapshot and issues a
random 256-bit token for a frozen subset. The token expires after at most five minutes, capped
by snapshot expiry. Preparing another review, dismissal, refresh, a filesystem scan, cleanup,
cancellation or app restart invalidates the relevant state. Failed refreshes leave no usable
old snapshot. None of this data is restored from localStorage.

`docker_check_review` consumes the token **before** collecting current state. It compares full
inspect metadata and references, then reports matching state or an error. Every outcome requires
a fresh inventory for another review. A match returns `cleanup_enabled: false` and `removed: 0`.
There is no Docker removal IPC or executor. The original `clean_noting` refusal is unchanged,
including for callers bypassing the new UI or having completed a successful review.

The collector now has a 30-second total deadline, 15-second per-command limits, a 4 MiB limit on
each output stream, a 32 MiB inventory-output budget and cancellation of its own process group.
The existing command runner retains its original unbounded/cancellation-free behaviour for other
callers. Docker CLI/context/configuration isolation is retained; the application uses Docker
Desktop's absolute CLI path. A debug UI test must inject an absolute fake executable or refuse.
The debug harness also limits its filesystem scan to the throwaway home.

The endpoint-name check is still not proof of socket ownership, daemon provenance or a pinned
transport. This read-only flow is not an atomic snapshot or future deletion authority. Container
start/finish times, image-platform details and a validated raw Engine/cache transport remain
follow-up work; the full metadata is retained privately for comparison in the meantime.

## Read-only implementation validation

- Core suite: **73 passed** (29 unit, 11 mocked Docker integration, 33 existing scan integration),
  including single-use tokens, forged/replayed tokens, expiry using injected monotonic instants,
  stale snapshots, dismissal/supersession, both output streams' limits, pre-cancelled commands and
  cancellation after the child actually starts. No failed or ignored tests.
- Native debug Tauri harness: `scripts/test-docker-ui.py` launches a temporary test app with a
  throwaway home and absolute fake CLI. Stable review, changed tags, CLI failure, empty inventory
  and cancellation all passed. Real IPC rejects unknown/duplicate keys and forged/replayed tokens;
  filesystem rescans invalidate reviews. The original cleanup IPC still refuses Docker removal
  after a successful recheck. Fake `Docker.raw` survives; only allowlisted reads reach the mock.
- Visually inspected native WebKit snapshots at **920 × 900** and the supported minimum
  **720 × 560**. Checked panel top/bottom, expanded details, the review list at both scroll ends,
  normal-size review and minimum-size failure/empty/changed/cancelled states. Long IDs and labels
  wrap, controls stay inside the window, and the last selected object can be scrolled fully into
  view. The small review now includes an object count and a conditional scroll hint; a new review
  starts at the top. No page/dialog horizontal overflow, webview errors or unhandled rejections.
- Captures come from `WKWebView.takeSnapshot` in the running Tauri app after finite CSS fades are
  finished. The desktop was locked, so this verifies native application content and layout rather
  than macOS window chrome. The debug-only capture command requires the UI harness and an absolute
  output path supplied at launch; release builds refuse capture. The existing WebKit dependency
  is now explicit for this helper; no package versions changed. Accessibility focus and a live
  Docker engine remain unverified.
- Workspace Clippy with warnings denied, formatting, JavaScript syntax and diff checks passed.
  Debug native build and release-profile compilation passed. No test or safety guard was removed,
  narrowed or weakened. Hosted CI status is reported separately in the PR.

Reproduce after `cargo +stable build --locked -p storage-app`:

```sh
python3 scripts/test-docker-ui.py --scenario stable --output /tmp/storage-docker-stable.json
python3 scripts/test-docker-ui.py --scenario changed --output /tmp/storage-docker-changed.json
python3 scripts/test-docker-ui.py --scenario failure --output /tmp/storage-docker-failure.json
python3 scripts/test-docker-ui.py --scenario empty --output /tmp/storage-docker-empty.json
python3 scripts/test-docker-ui.py --scenario cancel --output /tmp/storage-docker-cancel.json
```

Native visual reproduction (add `--visual-state review-bottom` to capture the last object):

```sh
python3 scripts/test-docker-ui.py --scenario stable --width 720 --height 560 --visual-state review --snapshot /tmp/storage-docker-min-review.png --output /tmp/storage-docker-min-review.json
```

Removal enablement still requires a proven bounded removal primitive; repeating the read-only
check does not meet that requirement.
