#!/usr/bin/env python3
"""Scan the home folder and build an interactive storage report (report.html).

Read-only: nothing is deleted. The report builds a cleanup script you review and run yourself.

Usage: python3 scan.py [--no-open] [--out PATH]
"""
from __future__ import annotations

import argparse
import json
import os
import plistlib
import re
import shlex
import stat
import subprocess
import sys
import time
from datetime import datetime

HOME = os.path.expanduser("~")
HERE = os.path.dirname(os.path.abspath(__file__))

MIN_NODE = 8_000_000          # smaller items are grouped into one tile
MAX_CHILDREN = 40             # tiles kept per folder
MAX_TREE_DEPTH = 7            # folder depth kept for the explorer
MIN_ENTRY = 1_000_000         # smaller cleanup candidates are dropped
LARGE_FILE = 1_000_000_000
LARGE_DOWNLOAD = 300_000_000
INSTALLER_MIN = 20_000_000

# Cleanup recommendations. "safe" = regenerated automatically; "review" = you decide.
RECS = [
    dict(id="npm", tier="safe", title="npm cache", hint="~/.npm/_cacache",
         desc="Package tarballs npm downloaded before. npm fetches them again when a project needs them."),
    dict(id="yarn", tier="safe", title="Yarn cache", hint="~/Library/Caches/Yarn",
         desc="Yarn's download cache. Re-downloaded on the next install."),
    dict(id="pnpm", tier="safe", title="pnpm store", hint="~/Library/pnpm/store",
         desc="pnpm's shared package store. Projects re-link packages on the next install."),
    dict(id="bun", tier="safe", title="Bun cache", hint="~/.bun/install/cache",
         desc="Bun's package download cache."),
    dict(id="pip", tier="safe", title="pip cache", hint="~/Library/Caches/pip",
         desc="Wheels pip downloaded before."),
    dict(id="uv", tier="safe", title="uv cache", hint="~/.cache/uv",
         desc="uv's package and Python download cache."),
    dict(id="homebrew", tier="safe", title="Homebrew downloads", hint="~/Library/Caches/Homebrew",
         desc="Bottles and source archives Homebrew already installed. Same as brew cleanup --prune=all."),
    dict(id="cocoapods", tier="safe", title="CocoaPods cache", hint="~/Library/Caches/CocoaPods",
         desc="Pod download cache. pod install fetches what it needs."),
    dict(id="go", tier="safe", title="Go build & module cache", hint="~/Library/Caches/go-build, ~/go/pkg/mod",
         desc="Compiled packages and downloaded modules. Same as go clean -cache -modcache."),
    dict(id="cargo", tier="safe", title="Cargo registry", hint="~/.cargo/registry, ~/.cargo/git",
         desc="Downloaded crates. Cargo fetches them again on the next build."),
    dict(id="gradle", tier="safe", title="Gradle caches", hint="~/.gradle/caches",
         desc="Dependency and build caches, Gradle distributions and daemon logs."),
    dict(id="maven", tier="safe", title="Maven repository", hint="~/.m2/repository",
         desc="Downloaded Java dependencies."),
    dict(id="conda", tier="safe", title="Conda package cache", hint="~/miniconda3/pkgs",
         desc="Package archives conda keeps after installing. Same as conda clean --all."),
    dict(id="browsers", tier="safe", title="Puppeteer & Playwright browsers", hint="~/.cache/puppeteer, ms-playwright",
         desc="Browser builds downloaded for testing. Re-downloaded when tests need them."),
    dict(id="xcode_dd", tier="safe", title="Xcode DerivedData", hint="~/Library/Developer/Xcode/DerivedData",
         desc="Build products and indexes, including XcodeBuildMCP workspaces. Rebuilt on the next build, which is slower."),
    dict(id="xcode_ds", tier="safe", title="Xcode device support", hint="~/Library/Developer/Xcode/*DeviceSupport",
         desc="Debug symbols copied from devices you've plugged in. Copied again when you reconnect a device."),
    dict(id="xcode_misc", tier="safe", title="Xcode previews & simulator caches",
         hint="~/Library/Developer/Xcode/UserData/Previews",
         desc="SwiftUI preview simulators and simulator caches."),
    dict(id="build", tier="safe", title="Build output in projects", hint=".next, .turbo, target, build",
         desc="Framework build folders inside your projects. Recreated the next time you build or run dev."),
    dict(id="oldversions", tier="safe", title="Old tool versions", hint="~/.codex/packages/*/releases",
         desc="Previous releases kept after an auto-update. The version in use (the one current points to) is never included."),
    dict(id="appcaches", tier="safe", title="App caches", hint="~/Library/Caches",
         desc="Caches apps keep to load faster. Quit the apps first; they rebuild what they need."),
    dict(id="electron", tier="safe", title="Chat & editor app caches", hint="~/Library/Application Support/*/Cache",
         desc="Web caches inside apps like Slack, Discord, VS Code and Chrome. Quit those apps first."),
    dict(id="containercaches", tier="safe", title="Sandboxed app caches",
         hint="~/Library/Containers/*/Data/Library/Caches",
         desc="Caches of App Store apps. Quit the apps first."),
    dict(id="toolcaches", tier="safe", title="Other tool caches", hint="~/.cache",
         desc="Caches written by command-line tools."),
    dict(id="logs", tier="safe", title="Logs", hint="~/Library/Logs",
         desc="Diagnostic logs from apps."),

    dict(id="android", tier="review", title="Android emulators & system images",
         hint="~/.android/avd, ~/Library/Android/sdk/system-images",
         desc="Emulator disks and the OS images they boot from. Delete the ones you no longer test on; Android Studio can download them again."),
    dict(id="simulators", tier="review", title="iOS simulators", hint="~/Library/Developer/CoreSimulator/Devices",
         desc="Each simulator keeps its own disk with installed apps and data. Removed with xcrun simctl delete."),
    dict(id="xcode_archives", tier="review", title="Xcode archives", hint="~/Library/Developer/Xcode/Archives",
         desc="App builds you archived for release. Keep the ones you may need symbols for."),
    dict(id="docker", tier="review", title="Docker images & build cache",
         hint="~/Library/Containers/com.docker.docker", approx=True,
         desc="Docker's disk image. The script runs docker system prune -a, which removes stopped containers, unused images and build cache, but keeps volumes. Docker must be running."),
    dict(id="ollama", tier="review", title="Ollama models", hint="~/.ollama/models",
         desc="Local language models. Removed with ollama rm; pull them again any time."),
    dict(id="mlmodels", tier="review", title="Hugging Face & PyTorch downloads", hint="~/.cache/huggingface",
         desc="Model weights and datasets downloaded by Python libraries. Downloaded again the next time code loads them."),
    dict(id="lmstudio", tier="review", title="LM Studio models", hint="~/.lmstudio/models",
         desc="Models downloaded in LM Studio."),
    dict(id="node_modules", tier="review", title="node_modules in projects", hint="*/node_modules",
         desc="Installed dependencies. Run your package manager's install to get them back. Idle projects are the easy wins."),
    dict(id="venv", tier="review", title="Python virtual environments", hint=".venv, venv",
         desc="Installed Python packages per project. Recreate them from requirements or pyproject."),
    dict(id="toolchains", tier="review", title="Node, Python & Rust versions", hint="~/.nvm, ~/.pyenv, ~/.rustup",
         desc="Language versions installed by version managers. Keep the ones your projects use."),
    dict(id="worktrees", tier="review", title="Agent worktrees", hint="*/.claude/worktrees",
         desc="Extra checkouts made by coding agents. Commit or push anything you need first. Run git worktree prune in the repo afterwards."),
    dict(id="codextasks", tier="review", title="Codex task folders", hint="~/Documents/Codex/<date>/<task>",
         desc="Working folders Codex created for each task. Keep anything you still need; the rest is safe to delete."),
    dict(id="trash", tier="review", title="Trash", hint="~/.Trash",
         desc="Files you already deleted. The script empties the Trash through Finder."),
    dict(id="iosbackups", tier="review", title="iPhone & iPad backups",
         hint="~/Library/Application Support/MobileSync/Backup",
         desc="Local device backups. Keep the latest one unless you back up to iCloud."),
    dict(id="installers", tier="review", title="Installers & disk images", hint=".dmg, .pkg, .iso, .xip",
         desc="Usually not needed once the app is installed."),
    dict(id="largefiles", tier="review", title="Large files", hint="1 GB+, or 300 MB+ in Downloads",
         desc="Big files outside Library. Check each one before you delete it."),
    dict(id="messages", tier="review", title="Messages attachments", hint="~/Library/Messages/Attachments",
         manual="Open Messages, then Settings, General, and set Keep Messages to 1 year. You can also go to System Settings, General, Storage, Messages and review large attachments.",
         desc="Photos, videos and files people sent you. Don't delete these folders directly; Messages manages them."),
]

FIXED = {
    ".npm/_cacache": "npm", ".npm/_npx": "npm",
    "Library/Caches/Yarn": "yarn", ".yarn/berry/cache": "yarn",
    "Library/pnpm/store": "pnpm",
    ".bun/install/cache": "bun",
    "Library/Caches/pip": "pip", ".cache/pip": "pip",
    ".cache/uv": "uv",
    "Library/Caches/Homebrew": "homebrew",
    "Library/Caches/CocoaPods": "cocoapods",
    "Library/Caches/go-build": "go", "go/pkg/mod": "go",
    ".cargo/registry": "cargo", ".cargo/git": "cargo",
    ".gradle/caches": "gradle", ".gradle/wrapper/dists": "gradle", ".gradle/daemon": "gradle",
    ".m2/repository": "maven",
    "miniconda3/pkgs": "conda", "anaconda3/pkgs": "conda", "miniforge3/pkgs": "conda",
    "opt/anaconda3/pkgs": "conda", "opt/miniconda3/pkgs": "conda",
    ".cache/puppeteer": "browsers", ".cache/ms-playwright": "browsers",
    "Library/Caches/ms-playwright": "browsers", "Library/Caches/ms-playwright-go": "browsers",
    "Library/Developer/Xcode/DerivedData": "xcode_dd",
    "Library/Developer/Xcode/iOS DeviceSupport": "xcode_ds",
    "Library/Developer/Xcode/watchOS DeviceSupport": "xcode_ds",
    "Library/Developer/Xcode/tvOS DeviceSupport": "xcode_ds",
    "Library/Developer/Xcode/macOS DeviceSupport": "xcode_ds",
    "Library/Developer/Xcode/visionOS DeviceSupport": "xcode_ds",
    "Library/Developer/Xcode/UserData/Previews": "xcode_misc",
    "Library/Developer/CoreSimulator/Caches": "xcode_misc",
    "Library/Developer/Xcode/Archives": "xcode_archives",
    "Library/Logs": "logs",
    ".Trash": "trash",
    "Library/Messages/Attachments": "messages",
    ".cache/torch": "mlmodels", ".cache/huggingface/xet": "mlmodels",
}
# Folders handled by a more specific rule below them; never claimed as a whole.
SKIP = {".cache/huggingface", ".cache/lm-studio", ".cache/huggingface/hub"}
CHILDREN_OF = {
    "Library/Caches": "appcaches",
    ".cache": "toolcaches",
    ".cache/huggingface/hub": "mlmodels",
    "Library/Developer/CoreSimulator/Devices": "simulators",
    "Library/Android/sdk/system-images": "android",
    ".android/avd": "android",
    "Library/Application Support/MobileSync/Backup": "iosbackups",
    "Library/Developer/XcodeBuildMCP/workspaces": "xcode_dd",
    ".nvm/versions/node": "toolchains",
    ".pyenv/versions": "toolchains",
    ".rustup/toolchains": "toolchains",
}
GRANDCHILDREN_OF = {".lmstudio/models": "lmstudio", ".cache/lm-studio/models": "lmstudio",
                    "Documents/Codex": "codextasks"}
# Caches macOS manages or protects itself.
APPLE_CACHES = ("com.apple.", "CloudKit", "com.crashlytics", "GeoServices", "PassKit", "Animoji",
                "FamilyCircle", "SiriTTS", "familycircled")
FIXED_FILES = {
    "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw": "docker",
}
CMDS = {
    "trash": "osascript -e 'tell application \"Finder\" to empty trash'",
    "docker": "docker system prune -a -f",
}
TOOLCHAIN_NAMES = {".nvm/versions/node": "Node", ".pyenv/versions": "Python", ".rustup/toolchains": "Rust"}

ELECTRON_CACHE = {"Cache", "Code Cache", "GPUCache", "CachedData", "CachedExtensionVSIXs", "DawnCache",
                  "DawnGraphiteCache", "DawnWebGPUCache", "GrShaderCache", "ShaderCache", "PersistentCache"}
CONTAINER_CACHE = re.compile(r"^Library/Containers/([^/]+)/Data/Library/Caches$")
BUILD_DIRS = {".next", ".turbo", ".nuxt", ".svelte-kit", ".parcel-cache", ".angular", ".docusaurus",
              ".expo", ".wrangler", ".dart_tool"}
GRADLE_FILES = {"build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"}
PROJECT_MARKERS = {".git", "package.json", "Cargo.toml", "pyproject.toml", "go.mod", "Package.swift",
                   "build.gradle", "build.gradle.kts", "Gemfile", "pubspec.yaml", "requirements.txt"}
INSTALLER_EXT = (".dmg", ".pkg", ".iso", ".xip", ".mpkg")
BUNDLE_EXT = (".app", ".photoslibrary", ".musiclibrary", ".tvlibrary", ".imovielibrary", ".fcpbundle",
              ".logicx", ".band", ".xcarchive", ".bundle", ".framework", ".sparsebundle", ".pages",
              ".numbers", ".key", ".photoslibrary")
MEDIA_BUNDLES = (".photoslibrary", ".musiclibrary", ".tvlibrary", ".imovielibrary", ".fcpbundle", ".logicx", ".band")
MEDIA_EXT = {".jpg", ".jpeg", ".png", ".heic", ".gif", ".tif", ".tiff", ".raw", ".cr2", ".nef", ".arw", ".dng",
             ".psd", ".webp", ".mov", ".mp4", ".m4v", ".mkv", ".avi", ".webm", ".mp3", ".m4a", ".wav", ".aiff",
             ".flac", ".aac", ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".key", ".pages",
             ".numbers", ".epub", ".zip", ".rar", ".7z"}

CAT_PATHS = {
    "Documents": "media", "Pictures": "media", "Movies": "media", "Music": "media",
    "Developer": "code", "Applications": "other",
    ".ollama": "ai", ".lmstudio": "ai", ".cache/huggingface": "ai", ".cache/lm-studio": "ai", ".cache/torch": "ai",
    ".npm": "cache", ".cache": "cache", ".gradle": "cache", ".cargo": "cache", ".m2": "cache", ".bun": "cache",
    ".yarn": "cache", ".swiftpm": "cache", ".pub-cache": "cache", ".cocoapods": "cache", "go": "cache",
    "Library/Caches": "cache", "Library/pnpm": "cache",
    "Library/Developer": "sdk", "Library/Android": "sdk", ".android": "sdk", ".rustup": "sdk", ".nvm": "sdk",
    ".pyenv": "sdk", ".homebrew": "sdk", ".local": "sdk", "Library/Containers/com.docker.docker": "sdk",
    "Library/Messages": "media", "Library/Application Support/MobileSync": "media",
    "Library/Mobile Documents": "media", "Library/CloudStorage": "media",
}

# iCloud and other File Provider folders can block for minutes when listed, so each child is
# measured with du under a time limit instead of being walked.
CLOUD_ROOTS = {"Library/Mobile Documents", "Library/CloudStorage"}
CLOUD_TIMEOUT = 15
CLOUD_BUDGET = 90

UUID = re.compile(r"^[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}$", re.I)


def read_plist(path):
    try:
        with open(path, "rb") as f:
            return plistlib.load(f)
    except Exception:
        return {}


def fmt_gb(n):
    return f"{n / 1e9:.1f} GB"


class Scanner:
    def __init__(self):
        self.entries = []
        self.labels = {}
        self.unreadable = []
        self.seen = set()
        self.files = 0
        self.bytes = 0
        self.dev = os.lstat(HOME).st_dev
        self.now = time.time()
        self.last_progress = 0.0

    # ---- matching -------------------------------------------------------------

    def match_dir(self, crel, prel, name, siblings, pat_ok, activity):
        if crel in SKIP:
            return None
        rid = FIXED.get(crel)
        if rid:
            info = {"cmd": CMDS[rid]} if rid in CMDS else {}
            return rid, info
        rid = CHILDREN_OF.get(prel)
        if rid:
            if rid == "appcaches" and name.startswith(APPLE_CACHES):
                return None
            return self.child_info(rid, crel, prel, name)
        pp, _, pname = prel.rpartition("/")
        rid = GRANDCHILDREN_OF.get(pp)
        if rid:
            return rid, {"label": f"{pname}/{name}"}
        m = CONTAINER_CACHE.match(crel)
        if m and not m.group(1).startswith(APPLE_CACHES):
            return "containercaches", {"label": m.group(1)}
        if pname == "releases":
            current = os.path.join(HOME, pp, "current")
            if os.path.islink(current) and os.path.realpath(current) != os.path.realpath(os.path.join(HOME, crel)):
                return "oldversions", {"label": f"{pp.split('/')[-1] if pp.count('/') else pp}: {name}"}
        if (prel.startswith("Library/Application Support/") and name in ELECTRON_CACHE
                and not prel.startswith("Library/Application Support/MobileSync")):
            app = crel.split("/")[2]
            return "electron", {"label": f"{app}: {name}"}
        if pname == "worktrees":
            gp = pp.rpartition("/")[2]
            if gp.startswith(".") and gp != ".git":
                return "worktrees", {}
        if pat_ok:
            if name == "node_modules" and "package.json" in siblings:
                info = {"label": prel}
                if activity:
                    info["d"] = int((self.now - activity) // 86400)
                return "node_modules", info
            if (name in BUILD_DIRS
                    or (name == "target" and "Cargo.toml" in siblings)
                    or (name in ("build", ".gradle") and siblings & GRADLE_FILES)
                    or (name == ".build" and "Package.swift" in siblings)
                    or (name == "output" and pname == ".vercel")):
                return "build", {}
        return None

    def child_info(self, rid, crel, prel, name):
        full = os.path.join(HOME, crel)
        if rid == "simulators":
            if not UUID.match(name):
                return None
            dev = read_plist(os.path.join(full, "device.plist"))
            runtime = str(dev.get("runtime", "")).rpartition(".")[2].replace("-", " ", 1).replace("-", ".")
            label = " ".join(x for x in (dev.get("name"), f"({runtime})" if runtime else "") if x) or name
            return rid, {"label": label, "cmd": f"xcrun simctl delete {name}"}
        if rid == "toolchains":
            return rid, {"label": f"{TOOLCHAIN_NAMES[prel]} {name}"}
        if rid == "iosbackups":
            info = read_plist(os.path.join(full, "Info.plist"))
            label = info.get("Device Name") or name[:12]
            when = info.get("Last Backup Date")
            out = {"label": label}
            if isinstance(when, datetime):
                out["m"] = int(when.timestamp())
            return rid, out
        if rid == "android" and name.endswith(".avd"):
            return rid, {"label": f"Emulator {name[:-4]}"}
        if rid == "android":
            return rid, {"label": f"System image {name}"}
        return rid, {"label": name}

    # ---- walking --------------------------------------------------------------

    def progress(self, force=False):
        t = time.monotonic()
        if force or t - self.last_progress > 0.25:
            self.last_progress = t
            sys.stderr.write(f"\r  {self.files:,} files, {fmt_gb(self.bytes)} scanned ")
            sys.stderr.flush()

    def cloud(self, path, rel, cat):
        deadline = time.monotonic() + CLOUD_BUDGET
        kids = []
        try:
            names = sorted(os.listdir(path))
        except OSError:
            self.unreadable.append(rel)
            names = []
        for name in names:
            crel = f"{rel}/{name}"
            left = deadline - time.monotonic()
            size = None
            if left > 1:
                try:
                    out = subprocess.run(["du", "-sk", os.path.join(path, name)], capture_output=True, text=True,
                                         timeout=min(CLOUD_TIMEOUT, left)).stdout
                    size = int(out.split()[0]) * 1024 if out.strip() else None
                except (subprocess.TimeoutExpired, ValueError, IndexError):
                    pass
            if size is None:
                self.unreadable.append(crel + " (iCloud didn't respond)")
                continue
            self.bytes += size
            self.progress()
            kids.append({"n": name, "s": size, "t": cat or "media"})
        kids.sort(key=lambda k: -k["s"])
        node = {"n": os.path.basename(path), "s": sum(k["s"] for k in kids), "t": cat or "media"}
        keep = [k for k in kids[:MAX_CHILDREN] if k["s"] >= MIN_NODE]
        if keep:
            rest = kids[len(keep):]
            if rest:
                keep.append({"n": f"{len(rest)} smaller items", "s": sum(k["s"] for k in rest), "t": node["t"],
                             "o": len(rest)})
            node["c"] = keep
        return node

    def walk(self, path, rel, depth, cat, claimed, in_bundle, rec, pat_ok):
        if rel in CLOUD_ROOTS:
            return self.cloud(path, rel, cat)
        try:
            with os.scandir(path) as it:
                items = []
                for e in it:
                    try:
                        items.append((e, e.stat(follow_symlinks=False)))
                    except OSError:
                        pass
        except OSError:
            if len(self.unreadable) < 200:
                self.unreadable.append(rel)
            return {"n": os.path.basename(path), "s": 0, "t": cat or "other", "u": 1}

        names = {e.name for e, _ in items}
        if depth > 0 and cat in (None, "media", "other") and not in_bundle and (
                names & PROJECT_MARKERS or any(n.endswith(".xcodeproj") for n in names)):
            cat = "code"
        if rec is None and not claimed and pat_ok and "pyvenv.cfg" in names:
            rec = ("venv", {"label": rel})
        claim_kids = claimed or rec is not None

        activity = 0.0
        if not claim_kids and "node_modules" in names and "package.json" in names:
            for e, st in items:
                if e.name not in ("node_modules", ".DS_Store"):
                    activity = max(activity, st.st_mtime)
            try:
                activity = max(activity, os.stat(os.path.join(path, ".git", "index")).st_mtime)
            except OSError:
                pass

        total = 0
        kids = []
        catsizes = {}
        for e, st in items:
            name = e.name
            crel = f"{rel}/{name}" if rel else name
            mode = st.st_mode
            if stat.S_ISDIR(mode):
                if st.st_dev != self.dev:
                    continue
                ccat = CAT_PATHS.get(crel, cat)
                bundle = in_bundle or name.endswith(BUNDLE_EXT)
                if bundle and not in_bundle:
                    ccat = "media" if name.endswith(MEDIA_BUNDLES) else (ccat or "other")
                crec = None
                if not claim_kids:
                    crec = self.match_dir(crel, rel, name, names, pat_ok and not bundle, activity)
                kid_pat_ok = pat_ok if depth > 0 else not (name.startswith(".") or name in ("Library", "Applications"))
                node = self.walk(e.path, crel, depth + 1, ccat, claim_kids, bundle, crec, kid_pat_ok)
            elif stat.S_ISREG(mode):
                if st.st_nlink > 1:
                    key = (st.st_dev, st.st_ino)
                    if key in self.seen:
                        continue
                    self.seen.add(key)
                size = st.st_blocks * 512
                self.files += 1
                self.bytes += size
                if not self.files % 5000:
                    self.progress()
                fcat = cat or ("media" if os.path.splitext(name)[1].lower() in MEDIA_EXT else "other")
                node = {"n": name, "s": size, "t": fcat, "f": 1}
                if crel in self.labels:
                    node["l"] = self.labels[crel]
                if not claim_kids:
                    self.match_file(crel, name, size, st, in_bundle, depth)
            else:
                continue
            total += node["s"]
            catsizes[node["t"]] = catsizes.get(node["t"], 0) + node["s"]
            kids.append(node)

        node = {"n": os.path.basename(path) if rel else "~", "s": total,
                "t": cat or (max(catsizes, key=catsizes.get) if catsizes else "other")}
        if rel in self.labels:
            node["l"] = self.labels[rel]
        if depth < MAX_TREE_DEPTH and not in_bundle and kids:
            kids.sort(key=lambda k: -k["s"])
            keep = [k for k in kids[:MAX_CHILDREN] if k["s"] >= MIN_NODE]
            rest = kids[len(keep):]
            if keep:
                rest_size = sum(k["s"] for k in rest)
                if rest_size > 0:
                    rc = {}
                    for k in rest:
                        rc[k["t"]] = rc.get(k["t"], 0) + k["s"]
                    keep.append({"n": f"{len(rest)} smaller items", "s": rest_size,
                                 "t": max(rc, key=rc.get), "o": len(rest)})
                node["c"] = keep
        if rec and total >= MIN_ENTRY:
            rid, info = rec
            self.entries.append({"r": rid, "p": rel, "s": total, **info})
        return node

    def match_file(self, crel, name, size, st, in_bundle, depth):
        rid = FIXED_FILES.get(crel)
        if rid:
            self.entries.append({"r": rid, "p": crel, "s": size, "cmd": CMDS[rid], "approx": 1})
            return
        top = crel.split("/", 1)[0]
        if in_bundle or top.startswith(".") or top in ("Library", "Applications"):
            return
        ext = os.path.splitext(name)[1].lower()
        if ext in INSTALLER_EXT and size >= INSTALLER_MIN:
            self.entries.append({"r": "installers", "p": crel, "s": size, "m": int(st.st_mtime)})
        elif size >= LARGE_FILE or (top == "Downloads" and size >= LARGE_DOWNLOAD):
            self.entries.append({"r": "largefiles", "p": crel, "s": size, "m": int(st.st_mtime)})

    def ollama(self):
        mdir = os.path.join(HOME, ".ollama/models/manifests")
        for root, _, files in os.walk(mdir):
            for f in files:
                fp = os.path.join(root, f)
                parts = os.path.relpath(fp, mdir).split(os.sep)
                if len(parts) != 4:
                    continue
                host, ns, model, tag = parts
                if host == "registry.ollama.ai":
                    name = f"{model}:{tag}" if ns == "library" else f"{ns}/{model}:{tag}"
                else:
                    name = f"{host}/{ns}/{model}:{tag}"
                try:
                    with open(fp) as fh:
                        m = json.load(fh)
                except Exception:
                    continue
                layers = m.get("layers") or []
                if not layers:
                    continue
                big = max(layers, key=lambda l: l.get("size", 0))
                blob = ".ollama/models/blobs/" + big.get("digest", "").replace(":", "-")
                self.labels[blob] = name
                self.entries.append({"r": "ollama", "p": blob, "label": name,
                                     "s": sum(l.get("size", 0) for l in layers),
                                     "cmd": f"ollama rm {shlex.quote(name)}"})


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(HERE, "report.html"))
    ap.add_argument("--no-open", action="store_true", help="don't open the report when done")
    args = ap.parse_args()

    started = time.time()
    print(f"Scanning {HOME} (read-only)…", file=sys.stderr)
    sc = Scanner()
    sc.ollama()
    tree = sc.walk(HOME, "", 0, None, False, False, None, False)
    sc.progress(force=True)
    sys.stderr.write("\n")

    vfs = os.statvfs(HOME)
    try:
        computer = subprocess.run(["scutil", "--get", "ComputerName"], capture_output=True,
                                  text=True, timeout=5).stdout.strip()
    except Exception:
        computer = ""

    data = {
        "generated": int(time.time()),
        "seconds": round(time.time() - started, 1),
        "home": HOME,
        "computer": computer,
        "disk": {"total": vfs.f_blocks * vfs.f_frsize, "free": vfs.f_bavail * vfs.f_frsize},
        "scanned": tree["s"],
        "files": sc.files,
        "unreadable": sc.unreadable,
        "recs": RECS,
        "entries": sorted(sc.entries, key=lambda e: -e["s"]),
        "tree": tree,
    }

    with open(os.path.join(HERE, "template.html"), encoding="utf-8") as f:
        html = f.read()
    payload = json.dumps(data, separators=(",", ":"), ensure_ascii=False).replace("</", "<\\/")
    html = html.replace("/*__DATA__*/null", payload, 1)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write(html)

    tiers = {}
    tier_of = {r["id"]: r["tier"] for r in RECS if not r.get("manual")}
    for e in sc.entries:
        if e["r"] in tier_of:
            tiers[tier_of[e["r"]]] = tiers.get(tier_of[e["r"]], 0) + e["s"]
    print(f"Home folder: {fmt_gb(tree['s'])} in {sc.files:,} files ({data['seconds']} s)", file=sys.stderr)
    print(f"Safe to remove: {fmt_gb(tiers.get('safe', 0))}   Needs review: {fmt_gb(tiers.get('review', 0))}",
          file=sys.stderr)
    if sc.unreadable:
        print(f"{len(sc.unreadable)} folders couldn't be read (grant Full Disk Access to your terminal to include them).",
              file=sys.stderr)
    print(f"Report: {args.out}", file=sys.stderr)
    if not args.no_open:
        subprocess.run(["open", args.out])


if __name__ == "__main__":
    main()
