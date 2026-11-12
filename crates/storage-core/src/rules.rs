//! Rule tables: categories, recommendations and the paths they match.
//!
//! Matching happens in `scan::Scanner::match_dir`, in this priority order:
//! `SKIP` → `FIXED` → `CHILDREN_OF` → `GRANDCHILDREN_OF` → path patterns → project patterns.
//! Linked git worktrees and Python virtual environments are detected while their folder is listed.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// What a byte on disk is, for the overview bar. Order here is the order of the bar segments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cat {
    Worktrees,
    Agents,
    Code,
    Deps,
    Build,
    Caches,
    Toolchains,
    Models,
    Containers,
    Trash,
    #[default]
    Other,
    /// Outside the home folder and not dev-related: other apps, shared files.
    System,
}

impl Cat {
    pub const COUNT: usize = 12;
    pub const ALL: [Cat; Cat::COUNT] = [
        Cat::Worktrees,
        Cat::Agents,
        Cat::Code,
        Cat::Deps,
        Cat::Build,
        Cat::Caches,
        Cat::Toolchains,
        Cat::Models,
        Cat::Containers,
        Cat::Trash,
        Cat::Other,
        Cat::System,
    ];
}

/// `Safe` items are regenerated automatically and are deleted outright.
/// `Review` items are the user's call and are moved to the Trash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Safe,
    Review,
}

#[derive(Debug, Serialize)]
pub struct Rec {
    pub id: &'static str,
    pub tier: Tier,
    pub cat: Cat,
    pub title: &'static str,
    pub hint: &'static str,
    pub desc: &'static str,
    /// The size is an upper bound (Docker's disk image).
    #[serde(skip_serializing_if = "is_false")]
    pub approx: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

const fn safe(id: &'static str, cat: Cat, title: &'static str, hint: &'static str, desc: &'static str) -> Rec {
    Rec { id, tier: Tier::Safe, cat, title, hint, desc, approx: false }
}

const fn review(id: &'static str, cat: Cat, title: &'static str, hint: &'static str, desc: &'static str) -> Rec {
    Rec { id, tier: Tier::Review, cat, title, hint, desc, approx: false }
}

pub static RECS: &[Rec] = &[
    // ---- safe: regenerated automatically ------------------------------------------------
    safe(
        "build",
        Cat::Build,
        "Build output",
        ".next, .turbo, target, .build, build",
        "Build folders inside your projects and worktrees. Recreated the next time you build or run dev.",
    ),
    safe(
        "node_modules",
        Cat::Deps,
        "node_modules",
        "*/node_modules",
        "Installed JavaScript dependencies. Your package manager reinstalls them from the lockfile.",
    ),
    safe(
        "xcode_dd",
        Cat::Build,
        "Xcode DerivedData",
        "~/Library/Developer/Xcode/DerivedData",
        "Build products and indexes, including XcodeBuildMCP workspaces. Rebuilt on the next build, which is slower.",
    ),
    safe(
        "agentcache",
        Cat::Agents,
        "Agent logs & scratch files",
        "~/.claude/shell-snapshots, ~/.codex/log, ~/.t3/caches",
        "Shell snapshots, debug logs and temp files that coding agents write while they run. Recreated as needed.",
    ),
    safe(
        "oldversions",
        Cat::Agents,
        "Old agent CLI versions",
        "~/.codex/packages/*/releases, ~/.local/share/claude/versions",
        "Previous releases kept after an auto-update. The version in use is never included.",
    ),
    safe(
        "npm",
        Cat::Caches,
        "npm cache",
        "~/.npm/_cacache",
        "Package tarballs npm downloaded before. npm fetches them again when a project needs them.",
    ),
    safe(
        "yarn",
        Cat::Caches,
        "Yarn cache",
        "~/Library/Caches/Yarn",
        "Yarn's download cache. Re-downloaded on the next install.",
    ),
    safe(
        "pnpm",
        Cat::Caches,
        "pnpm store",
        "~/Library/pnpm/store",
        "pnpm's shared package store. Projects re-link packages on the next install.",
    ),
    safe("bun", Cat::Caches, "Bun cache", "~/.bun/install/cache", "Bun's package download cache."),
    safe("pip", Cat::Caches, "pip cache", "~/Library/Caches/pip", "Wheels pip downloaded before."),
    safe("uv", Cat::Caches, "uv cache", "~/.cache/uv", "uv's package and Python download cache."),
    safe(
        "homebrew",
        Cat::Caches,
        "Homebrew downloads",
        "~/Library/Caches/Homebrew",
        "Bottles and source archives Homebrew already installed. Same as brew cleanup --prune=all.",
    ),
    safe(
        "cocoapods",
        Cat::Caches,
        "CocoaPods cache",
        "~/Library/Caches/CocoaPods",
        "Pod download cache. pod install fetches what it needs.",
    ),
    safe(
        "go",
        Cat::Caches,
        "Go build & module cache",
        "~/Library/Caches/go-build, ~/go/pkg/mod",
        "Compiled packages and downloaded modules. Same as go clean -cache -modcache.",
    ),
    safe(
        "cargo",
        Cat::Caches,
        "Cargo registry",
        "~/.cargo/registry, ~/.cargo/git",
        "Downloaded crates. Cargo fetches them again on the next build.",
    ),
    safe(
        "gradle",
        Cat::Caches,
        "Gradle caches",
        "~/.gradle/caches",
        "Dependency and build caches, Gradle distributions and daemon logs.",
    ),
    safe("maven", Cat::Caches, "Maven repository", "~/.m2/repository", "Downloaded Java dependencies."),
    safe(
        "conda",
        Cat::Caches,
        "Conda package cache",
        "~/miniconda3/pkgs",
        "Package archives conda keeps after installing. Same as conda clean --all.",
    ),
    safe(
        "browsers",
        Cat::Caches,
        "Playwright & Puppeteer browsers",
        "~/.cache/puppeteer, ms-playwright",
        "Browser builds downloaded for testing and browser agents. Re-downloaded when they're needed.",
    ),
    safe(
        "xcode_ds",
        Cat::Toolchains,
        "Xcode device support",
        "~/Library/Developer/Xcode/*DeviceSupport",
        "Debug symbols copied from devices you've plugged in. Copied again when you reconnect a device.",
    ),
    safe(
        "xcode_misc",
        Cat::Caches,
        "Xcode previews & simulator caches",
        "~/Library/Developer/Xcode/UserData/Previews",
        "SwiftUI preview simulators and simulator caches.",
    ),
    safe(
        "appcaches",
        Cat::Caches,
        "App caches",
        "~/Library/Caches",
        "Caches apps keep to load faster. Quit the apps first; they rebuild what they need.",
    ),
    safe(
        "electron",
        Cat::Caches,
        "Editor & chat app caches",
        "~/Library/Application Support/*/Cache",
        "Web caches inside apps like Cursor, VS Code, Slack and Chrome. Quit those apps first.",
    ),
    safe(
        "containercaches",
        Cat::Caches,
        "Sandboxed app caches",
        "~/Library/Containers/*/Data/Library/Caches",
        "Caches of App Store apps. Quit the apps first.",
    ),
    safe("toolcaches", Cat::Caches, "Other tool caches", "~/.cache", "Caches written by command-line tools."),
    safe("logs", Cat::Caches, "Logs", "~/Library/Logs", "Diagnostic logs from apps."),
    // ---- review: you decide; moved to the Trash ----------------------------------------
    review(
        "worktrees",
        Cat::Worktrees,
        "Agent worktrees",
        "~/.t3/worktrees, .claude/worktrees, ~/.codex/worktrees",
        "Extra checkouts that coding agents create for each task. They go to the Trash; the branch and its commits stay in the repo, and git's record of the worktree is pruned.",
    ),
    review(
        "agentlogs",
        Cat::Agents,
        "Agent session history",
        "~/.claude/projects, ~/.codex/sessions",
        "Transcripts that let you resume or search past agent sessions. Older ones are usually fine to let go.",
    ),
    review(
        "codextasks",
        Cat::Agents,
        "Codex task folders",
        "~/Documents/Codex/<date>/<task>",
        "Working folders Codex created for each task. Keep anything you still need.",
    ),
    review(
        "venv",
        Cat::Deps,
        "Python virtual environments",
        ".venv, venv",
        "Installed Python packages per project. Recreate them from requirements or pyproject.",
    ),
    review(
        "toolchains",
        Cat::Toolchains,
        "Node, Python & Rust versions",
        "~/.nvm, ~/.pyenv, ~/.rustup",
        "Language versions installed by version managers. Keep the ones your projects use.",
    ),
    review(
        "simulators",
        Cat::Toolchains,
        "iOS simulators",
        "~/Library/Developer/CoreSimulator/Devices",
        "Each simulator keeps its own disk with installed apps and data. Removed with xcrun simctl delete.",
    ),
    review(
        "android",
        Cat::Toolchains,
        "Android emulators & system images",
        "~/.android/avd, ~/Library/Android/sdk/system-images",
        "Emulator disks and the OS images they boot from. Android Studio can download them again.",
    ),
    review(
        "xcode_archives",
        Cat::Build,
        "Xcode archives",
        "~/Library/Developer/Xcode/Archives",
        "App builds you archived for release. Keep the ones you may need symbols for.",
    ),
    Rec {
        approx: true,
        ..review(
            "docker",
            Cat::Containers,
            "Docker images & build cache",
            "~/Library/Containers/com.docker.docker",
            "Docker's disk image. Runs docker system prune -a, which removes stopped containers, unused images and build cache, but keeps volumes. Docker must be running.",
        )
    },
    review(
        "ollama",
        Cat::Models,
        "Ollama models",
        "~/.ollama/models",
        "Local language models. Removed with ollama rm; pull them again any time.",
    ),
    review(
        "mlmodels",
        Cat::Models,
        "Hugging Face & PyTorch downloads",
        "~/.cache/huggingface",
        "Model weights and datasets downloaded by Python libraries. Downloaded again the next time code loads them.",
    ),
    review("lmstudio", Cat::Models, "LM Studio models", "~/.lmstudio/models", "Models downloaded in LM Studio."),
    review(
        "trash",
        Cat::Trash,
        "Trash",
        "~/.Trash",
        "Files you already deleted. Emptying the Trash removes them for good.",
    ),
    review(
        "installers",
        Cat::Other,
        "Installers & disk images",
        ".dmg, .pkg, .iso, .xip",
        "Usually not needed once the app is installed.",
    ),
    review(
        "largefiles",
        Cat::Other,
        "Large files",
        "1 GB+, or 300 MB+ in Downloads",
        "Big files outside Library. Check each one first.",
    ),
    // ---- outside the home folder ---------------------------------------------------------
    safe(
        "agenttmp",
        Cat::Agents,
        "Agent temp files",
        "/tmp/claude-<uid>",
        "Task output and scratch files from past Claude Code sessions. Anything touched in the last day is left alone.",
    ),
    safe(
        "syscaches",
        Cat::Caches,
        "Per-user system caches",
        "/var/folders/…/C",
        "Caches apps keep outside your Library folder. Quit the apps first.",
    ),
    safe(
        "brewold",
        Cat::Toolchains,
        "Old Homebrew versions",
        "/opt/homebrew/Cellar",
        "Versions Homebrew kept after an upgrade. Removed with brew cleanup.",
    ),
    review(
        "tmpfiles",
        Cat::Caches,
        "Temporary files",
        "/tmp, /var/folders/…/T",
        "Files left in temporary folders and not touched for a day or more. Tools that stage big downloads or disk images here often leave them behind.",
    ),
    review(
        "simruntimes",
        Cat::Toolchains,
        "Simulator runtimes",
        "xcrun simctl runtime list",
        "iOS, watchOS, tvOS and visionOS runtimes for the simulator. Removed with xcrun simctl runtime delete; Xcode downloads them again from Settings, Components.",
    ),
    review(
        "oldxcode",
        Cat::Toolchains,
        "Other Xcode versions",
        "/Applications/Xcode*.app",
        "Xcode copies besides the one xcode-select points to.",
    ),
];

pub fn rec(id: &str) -> &'static Rec {
    RECS.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("unknown recommendation {id}"))
}

fn map<V: Copy>(pairs: &[(&'static str, V)]) -> HashMap<&'static str, V> {
    pairs.iter().copied().collect()
}

pub static FIXED: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    map(&[
        (".npm/_cacache", "npm"),
        (".npm/_npx", "npm"),
        ("Library/Caches/Yarn", "yarn"),
        (".yarn/berry/cache", "yarn"),
        ("Library/pnpm/store", "pnpm"),
        (".bun/install/cache", "bun"),
        ("Library/Caches/pip", "pip"),
        (".cache/pip", "pip"),
        (".cache/uv", "uv"),
        ("Library/Caches/Homebrew", "homebrew"),
        ("Library/Caches/CocoaPods", "cocoapods"),
        ("Library/Caches/go-build", "go"),
        ("go/pkg/mod", "go"),
        (".cargo/registry", "cargo"),
        (".cargo/git", "cargo"),
        (".gradle/caches", "gradle"),
        (".gradle/wrapper/dists", "gradle"),
        (".gradle/daemon", "gradle"),
        (".m2/repository", "maven"),
        ("miniconda3/pkgs", "conda"),
        ("anaconda3/pkgs", "conda"),
        ("miniforge3/pkgs", "conda"),
        ("opt/anaconda3/pkgs", "conda"),
        ("opt/miniconda3/pkgs", "conda"),
        (".cache/puppeteer", "browsers"),
        (".cache/ms-playwright", "browsers"),
        ("Library/Caches/ms-playwright", "browsers"),
        ("Library/Caches/ms-playwright-go", "browsers"),
        ("Library/Developer/Xcode/DerivedData", "xcode_dd"),
        ("Library/Developer/Xcode/iOS DeviceSupport", "xcode_ds"),
        ("Library/Developer/Xcode/watchOS DeviceSupport", "xcode_ds"),
        ("Library/Developer/Xcode/tvOS DeviceSupport", "xcode_ds"),
        ("Library/Developer/Xcode/macOS DeviceSupport", "xcode_ds"),
        ("Library/Developer/Xcode/visionOS DeviceSupport", "xcode_ds"),
        ("Library/Developer/Xcode/UserData/Previews", "xcode_misc"),
        ("Library/Developer/CoreSimulator/Caches", "xcode_misc"),
        ("Library/Developer/Xcode/Archives", "xcode_archives"),
        ("Library/Logs", "logs"),
        (".Trash", "trash"),
        (".cache/torch", "mlmodels"),
        (".cache/huggingface/xet", "mlmodels"),
        (".claude/shell-snapshots", "agentcache"),
        (".claude/debug", "agentcache"),
        (".claude/statsig", "agentcache"),
        (".codex/log", "agentcache"),
        (".codex/tmp", "agentcache"),
        (".codex/shell_snapshots", "agentcache"),
        (".t3/caches", "agentcache"),
        (".codex/archived_sessions", "agentlogs"),
    ])
});

/// Folders handled by a more specific rule below them; never claimed as a whole.
pub static SKIP: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| [".cache/huggingface", ".cache/lm-studio", ".cache/huggingface/hub"].into_iter().collect());

/// Each child of these folders is its own entry.
pub static CHILDREN_OF: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    map(&[
        ("Library/Caches", "appcaches"),
        (".cache", "toolcaches"),
        (".cache/huggingface/hub", "mlmodels"),
        ("Library/Developer/CoreSimulator/Devices", "simulators"),
        ("Library/Android/sdk/system-images", "android"),
        (".android/avd", "android"),
        ("Library/Developer/XcodeBuildMCP/workspaces", "xcode_dd"),
        (".nvm/versions/node", "toolchains"),
        (".pyenv/versions", "toolchains"),
        (".rustup/toolchains", "toolchains"),
        (".claude/projects", "agentlogs"),
        (".gemini/tmp", "agentlogs"),
        (".local/share/claude/versions", "oldversions"),
    ])
});

/// Each grandchild of these folders is its own entry, labelled `child/grandchild`.
pub static GRANDCHILDREN_OF: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    map(&[
        (".lmstudio/models", "lmstudio"),
        (".cache/lm-studio/models", "lmstudio"),
        ("Documents/Codex", "codextasks"),
        (".codex/sessions", "agentlogs"),
    ])
});

pub static FIXED_FILES: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| map(&[("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw", "docker")]));

/// Recommendations removed by their own tool rather than by deleting the path.
pub fn command(rec: &str) -> Option<Vec<String>> {
    match rec {
        "docker" => Some(["docker", "system", "prune", "-a", "-f"].map(String::from).to_vec()),
        _ => None,
    }
}

pub const TOOLCHAIN_NAMES: &[(&str, &str)] =
    &[(".nvm/versions/node", "Node"), (".pyenv/versions", "Python"), (".rustup/toolchains", "Rust")];

/// Caches macOS manages or protects itself.
pub const APPLE_CACHES: &[&str] = &[
    "com.apple.",
    "CloudKit",
    "com.crashlytics",
    "GeoServices",
    "PassKit",
    "Animoji",
    "FamilyCircle",
    "SiriTTS",
    "familycircled",
];
pub const ELECTRON_CACHE: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "CachedData",
    "CachedExtensionVSIXs",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GrShaderCache",
    "ShaderCache",
    "PersistentCache",
];
pub const BUILD_DIRS: &[&str] = &[
    ".next",
    ".turbo",
    ".nuxt",
    ".svelte-kit",
    ".parcel-cache",
    ".angular",
    ".docusaurus",
    ".expo",
    ".wrangler",
    ".dart_tool",
];
pub const GRADLE_FILES: &[&str] = &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "Package.swift",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "pubspec.yaml",
    "requirements.txt",
    "deno.json",
];
/// Folders that are byproducts, not sources; ignored when judging when a project was last touched.
pub const BYPRODUCTS: &[&str] = &["node_modules", ".DS_Store", "target", "build", ".build", ".venv", "venv", ".gradle"];
pub const INSTALLER_EXT: &[&str] = &["dmg", "pkg", "iso", "xip", "mpkg"];
pub const BUNDLE_EXT: &[&str] = &[
    "app",
    "photoslibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
    "fcpbundle",
    "logicx",
    "band",
    "xcarchive",
    "bundle",
    "framework",
    "sparsebundle",
    "pages",
    "numbers",
    "key",
];

/// iCloud and other File Provider folders can block for minutes when listed, so each child is
/// measured with `du` under a time limit instead of being walked.
pub const CLOUD_ROOTS: &[&str] = &["Library/Mobile Documents", "Library/CloudStorage"];

static CAT_PATHS: LazyLock<HashMap<&'static str, Cat>> = LazyLock::new(|| {
    use Cat::*;
    map(&[
        ("Developer", Code),
        ("Projects", Code),
        ("Code", Code),
        ("code", Code),
        ("src", Code),
        ("repos", Code),
        (".claude", Agents),
        (".codex", Agents),
        (".t3", Agents),
        (".cursor", Agents),
        (".gemini", Agents),
        (".grok", Agents),
        (".local/share/claude", Agents),
        ("conductor", Agents),
        ("Library/Application Support/Claude", Agents),
        ("Library/Application Support/Cursor", Agents),
        ("Library/Application Support/Windsurf", Agents),
        ("Library/Application Support/com.conductor.app", Agents),
        (".npm", Caches),
        (".cache", Caches),
        (".gradle", Caches),
        (".cargo", Caches),
        (".m2", Caches),
        (".bun", Caches),
        (".yarn", Caches),
        (".swiftpm", Caches),
        (".pub-cache", Caches),
        (".cocoapods", Caches),
        ("go", Caches),
        ("Library/Caches", Caches),
        ("Library/pnpm", Caches),
        ("Library/Logs", Caches),
        ("Library/Developer", Toolchains),
        ("Library/Android", Toolchains),
        (".android", Toolchains),
        (".rustup", Toolchains),
        (".nvm", Toolchains),
        (".pyenv", Toolchains),
        (".volta", Toolchains),
        (".fnm", Toolchains),
        (".asdf", Toolchains),
        (".sdkman", Toolchains),
        (".dotnet", Toolchains),
        (".deno", Toolchains),
        (".local/share/mise", Toolchains),
        (".local/share/uv", Toolchains),
        ("miniconda3", Toolchains),
        ("anaconda3", Toolchains),
        ("miniforge3", Toolchains),
        (".ollama", Models),
        (".lmstudio", Models),
        (".cache/huggingface", Models),
        (".cache/lm-studio", Models),
        (".cache/torch", Models),
        (".cache/whisper", Models),
        ("Library/Containers/com.docker.docker", Containers),
        (".docker", Containers),
        (".orbstack", Containers),
        (".colima", Containers),
        (".lima", Containers),
        ("Library/Containers/com.utmapp.UTM", Containers),
        ("Parallels", Containers),
        ("Virtual Machines.localized", Containers),
        ("Library/Group Containers/HUAQ24HBR6.dev.orbstack", Containers),
        (".Trash", Trash),
    ])
});

/// Category for a child folder. Anything inside a worktree counts as the worktree; otherwise a
/// matched recommendation wins, then a known path, then the parent's category.
pub fn child_cat(parent: Option<Cat>, rec_id: Option<&str>, crel: &str) -> Option<Cat> {
    if parent == Some(Cat::Worktrees) {
        return parent;
    }
    if let Some(id) = rec_id {
        return Some(rec(id).cat);
    }
    if let Some(app) = crel.strip_prefix("/Applications/").filter(|a| !a.contains('/')) {
        return Some(app_cat(app));
    }
    CAT_PATHS.get(crel).copied().or(parent)
}

/// Category for an app in /Applications: dev tools get their own segment, the rest is `System`.
pub fn app_cat(app: &str) -> Cat {
    let name = app.strip_suffix(".app").unwrap_or(app);
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if starts(&[
        "Xcode",
        "Android Studio",
        "Visual Studio Code",
        "Zed",
        "Sublime",
        "Nova",
        "IntelliJ",
        "PyCharm",
        "WebStorm",
        "RustRover",
        "GoLand",
        "CLion",
        "Fleet",
        "Tower",
        "Fork",
        "GitHub Desktop",
        "Simulator",
    ]) {
        Cat::Toolchains
    } else if starts(&["Claude", "Codex", "ChatGPT", "Cursor", "Windsurf", "Conductor", "T3 Code", "Warp", "Ghostty"]) {
        Cat::Agents
    } else if starts(&["Docker", "OrbStack", "UTM", "Parallels", "VirtualBox", "VMware"]) {
        Cat::Containers
    } else if starts(&["Ollama", "LM Studio", "Msty", "Jan"]) {
        Cat::Models
    } else {
        Cat::System
    }
}

/// Folders outside the home folder that are scanned, with their category and the label in the drill-down view.
/// The per-user temp and cache folders under /var/folders are added at run time.
pub const SYSTEM_ROOTS: &[(&str, Cat, &str)] = &[
    ("/Applications", Cat::System, "Applications"),
    ("/Library/Developer", Cat::Toolchains, "Developer tools"),
    ("/opt/homebrew", Cat::Toolchains, "Homebrew"),
    ("/usr/local", Cat::Toolchains, "/usr/local"),
    ("/private/tmp", Cat::Caches, "/tmp"),
    ("/Users/Shared", Cat::Other, "Shared"),
];

pub fn is_bundle(name: &str) -> bool {
    ext(name).is_some_and(|e| BUNDLE_EXT.contains(&e.as_str()))
}

pub fn ext(name: &str) -> Option<String> {
    let (stem, e) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| e.to_ascii_lowercase())
}

pub fn key(rec: &str, id: &str) -> String {
    format!("{rec}|{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_points_at_a_known_rec() {
        let ids =
            FIXED.values().chain(CHILDREN_OF.values()).chain(GRANDCHILDREN_OF.values()).chain(FIXED_FILES.values());
        for id in ids {
            rec(id);
        }
    }

    #[test]
    fn rec_ids_are_unique() {
        let ids: HashSet<_> = RECS.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), RECS.len());
    }

    #[test]
    fn worktrees_keep_their_category() {
        assert_eq!(child_cat(Some(Cat::Worktrees), Some("node_modules"), "x/node_modules"), Some(Cat::Worktrees));
        assert_eq!(child_cat(Some(Cat::Code), Some("node_modules"), "x/node_modules"), Some(Cat::Deps));
        assert_eq!(child_cat(Some(Cat::Caches), None, ".cache/torch"), Some(Cat::Models));
        assert_eq!(child_cat(None, None, "Documents"), None);
    }

    #[test]
    fn apps_by_kind() {
        assert_eq!(child_cat(Some(Cat::System), None, "/Applications/Xcode-26.app"), Some(Cat::Toolchains));
        assert_eq!(child_cat(Some(Cat::System), None, "/Applications/T3 Code (Nightly).app"), Some(Cat::Agents));
        assert_eq!(child_cat(Some(Cat::System), None, "/Applications/Safari.app"), Some(Cat::System));
        assert_eq!(child_cat(Some(Cat::Toolchains), None, "/Applications/Xcode.app/Contents"), Some(Cat::Toolchains));
    }
}
