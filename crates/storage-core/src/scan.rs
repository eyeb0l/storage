//! A single parallel pass over the home folder and the dev-relevant folders outside it.
//!
//! Sizes are allocated bytes (`st_blocks * 512`), hard links are counted once, other volumes and
//! symlinks are skipped. Every byte gets a [`Cat`] for the overview bar, and folders that match a
//! rule in [`rules`] become cleanup [`Entry`]s. Once a folder matches, nothing beneath it can match
//! again, so entries don't overlap. The one exception is a worktree: its build output and
//! dependencies are entries too, with `parent` set to the worktree's key.
//!
//! Paths are home-relative for the home folder (`Developer/app`) and absolute outside it
//! (`/Applications/Xcode.app`). The walk also builds a pruned folder [`Node`] tree for the drill-down view.

use crate::bulk::{self, Item, Kind};
use crate::git::{self, Worktree};
use crate::rules::{self, Cat};
use crate::sys::{self, Disk};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MIN_ENTRY: u64 = 1_000_000;
/// Smaller items are grouped into one "N smaller items" node in the tree.
const MIN_NODE: u64 = 8_000_000;
/// Tree nodes kept per folder.
const MAX_CHILDREN: usize = 40;
/// Folder depth kept in the tree, counted from each scan root.
const MAX_TREE_DEPTH: u32 = 8;
const LARGE_FILE: u64 = 1_000_000_000;
const LARGE_DOWNLOAD: u64 = 300_000_000;
const INSTALLER_MIN: u64 = 20_000_000;
const MAX_UNREADABLE: usize = 200;
const CLOUD_TIMEOUT: Duration = Duration::from_secs(15);
const CLOUD_BUDGET: Duration = Duration::from_secs(60);
const DAY: i64 = 86_400;
/// How long something in Downloads has to sit untouched before it's offered, and how big it has to be.
const OLD_DOWNLOAD: i64 = 90 * DAY;
const DOWNLOAD_MIN: u64 = 5_000_000;
/// Folders in Application Support and Caches that aren't named after their app.
const APP_ALIASES: &[(&str, &str)] =
    &[("code", "com.microsoft.vscode"), ("google", "com.google.chrome"), ("bravesoftware", "com.brave.browser")];
/// The walk waits on the kernel far more than it computes, so more threads than cores still helps.
const THREADS: usize = 16;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Entry {
    pub key: String,
    pub rec: &'static str,
    /// Home-relative path, or absolute outside the home folder. Use `abs` to act on it.
    pub path: String,
    pub size: u64,
    pub cat: Cat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Days since the project around it (or for temp files, anything in it) was last touched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    /// Removed by running this instead of deleting the path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,
    /// Why this shouldn't be cleared without a second look ("app is running", "default version").
    /// Never suggested or swept up by a group selection; the user can still pick it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// A folder inside that is kept when the entry is cleared, and isn't counted in `size`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spare: Option<String>,
    /// Key of the worktree this lives in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    #[serde(skip)]
    pub abs: PathBuf,
    #[serde(skip)]
    pub gitdir: Option<PathBuf>,
    /// Device and inode when scanned. Cleanup refuses anything else that has since taken the path.
    #[serde(skip)]
    pub ident: Option<(u64, u64)>,
    /// When it was scanned, unix seconds.
    #[serde(skip)]
    pub scanned: i64,
    /// Offered only because nothing inside had changed for this long; checked again before clearing.
    #[serde(skip)]
    pub min_age: i64,
}

/// A folder or file in the drill-down tree. Keys are short because the tree is sent to the UI whole.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Node {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "t")]
    pub cat: Cat,
    #[serde(rename = "c", skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// Path of a scan root (`""` for home, absolute otherwise). Deeper paths are built from names.
    #[serde(rename = "p", skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "f", skip_serializing_if = "is_false")]
    pub file: bool,
    /// Stands for this many smaller items.
    #[serde(rename = "o", skip_serializing_if = "is_zero")]
    pub others: u32,
    #[serde(rename = "u", skip_serializing_if = "is_false")]
    pub unreadable: bool,
    /// Not a folder on disk (simulator runtimes, the part of the disk that wasn't scanned).
    #[serde(rename = "x", skip_serializing_if = "is_false")]
    pub virt: bool,
    /// Newest modification time of it or anything inside, folders included.
    #[serde(skip)]
    pub newest: i64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub generated: i64,
    pub seconds: f64,
    pub home: String,
    pub computer: String,
    pub disk: Disk,
    pub scanned: u64,
    pub files: u64,
    pub cats: BTreeMap<Cat, u64>,
    pub recs: &'static [rules::Rec],
    pub entries: Vec<Entry>,
    /// The first folders that couldn't be read (at most `MAX_UNREADABLE`).
    pub unreadable: Vec<String>,
    /// How many couldn't be read in all.
    pub unreadable_total: u64,
    /// Whether macOS let the scan into protected folders (Full Disk Access).
    pub full_access: bool,
    /// Whether the Trash could be read, and so can be emptied from here.
    pub trash_readable: bool,
    pub tree: Node,
}

#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub files: u64,
    pub bytes: u64,
    pub path: String,
    pub cats: BTreeMap<Cat, u64>,
}

#[derive(Default)]
struct Hit {
    rec: &'static str,
    label: Option<String>,
    idle_days: Option<u32>,
    mtime: Option<i64>,
    cmd: Option<Vec<String>>,
    gitdir: Option<PathBuf>,
    /// Only an entry if nothing inside changed for this many seconds; the idle time comes from the contents.
    min_age: i64,
    /// Only an entry at this size or more (never less than `MIN_ENTRY`).
    min_size: u64,
    hold: Option<String>,
    /// A child folder that is kept and not counted.
    spare: Option<&'static str>,
}

impl Hit {
    fn new(rec: &'static str) -> Self {
        Hit { rec, cmd: rules::command(rec), ..Default::default() }
    }
    fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

#[derive(Clone)]
struct Ctx {
    cat: Option<Cat>,
    /// An ancestor already matched a rule.
    claimed: bool,
    in_bundle: bool,
    /// Project patterns (node_modules, target, …) may match here.
    pat_ok: bool,
    /// Key of the enclosing worktree entry.
    wt: Option<Arc<str>>,
    depth: u32,
    /// Depth below the scan root, for pruning the tree.
    tree_depth: u32,
    /// Last time the parent project was touched, unix seconds (0 if unknown).
    activity: i64,
    /// Outside the home folder: unreadable folders are expected (root-owned) and not reported.
    quiet: bool,
    /// When this folder itself last changed (something added, removed or renamed in it), unix
    /// seconds. 0 at a scan root.
    mtime: i64,
}

struct Root {
    path: PathBuf,
    rel: String,
    cat: Cat,
    label: String,
}

/// What was running when the scan started.
#[derive(Default)]
struct Live {
    /// Bundle identifiers and names of running apps, lowercased.
    running: HashSet<String>,
    /// Working directories of running processes, if the system could be asked.
    cwds: Option<Vec<PathBuf>>,
}

pub struct Scanner {
    home: PathBuf,
    dev: u64,
    uid: u32,
    now: i64,
    files: AtomicU64,
    bytes: AtomicU64,
    cats: [AtomicU64; Cat::COUNT],
    entries: Mutex<Vec<Entry>>,
    unreadable: Mutex<Vec<String>>,
    seen: Mutex<HashSet<(u64, u64)>>,
    current: Mutex<String>,
    claude_current: Option<PathBuf>,
    xcode: Option<PathBuf>,
    user_temp: Option<String>,
    user_cache: Option<String>,
    /// What's running: asked on its own thread while the walk starts, since `lsof` takes a moment.
    probe: Mutex<Option<std::thread::JoinHandle<Live>>>,
    live: std::sync::OnceLock<Live>,
    /// Home-relative folders of the language versions set as default (rustup, nvm, pyenv).
    defaults: HashSet<String>,
    unreadable_total: AtomicU64,
    trash_unreadable: AtomicBool,
    /// Scan only the home folder (tests).
    home_only: bool,
    cancelled: AtomicBool,
}

impl Scanner {
    pub fn new(home: impl Into<PathBuf>) -> io::Result<Self> {
        let home = home.into();
        let dev = fs::metadata(&home)?.dev();
        let claude_current = fs::canonicalize(home.join(".local/bin/claude")).ok();
        let dir = |name| sys::darwin_dir(name).map(|p| p.display().to_string());
        let defaults = default_toolchains(&home);
        Ok(Scanner {
            defaults,
            home,
            dev,
            uid: sys::uid(),
            now: sys::now(),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            cats: std::array::from_fn(|_| AtomicU64::new(0)),
            entries: Mutex::default(),
            unreadable: Mutex::default(),
            seen: Mutex::default(),
            current: Mutex::default(),
            claude_current,
            xcode: sys::xcode_app(),
            user_temp: dir("DARWIN_USER_TEMP_DIR"),
            user_cache: dir("DARWIN_USER_CACHE_DIR"),
            probe: Mutex::new(Some(std::thread::spawn(|| Live {
                running: sys::running_apps(),
                cwds: sys::process_cwds(),
            }))),
            live: std::sync::OnceLock::new(),
            unreadable_total: AtomicU64::new(0),
            trash_unreadable: AtomicBool::new(false),
            home_only: false,
            cancelled: AtomicBool::new(false),
        })
    }

    /// Skips everything outside the home folder. For tests against a fake home.
    pub fn home_only(mut self) -> Self {
        self.home_only = true;
        self
    }

    /// Stops a running scan early. `run` still returns, with whatever it had counted; check
    /// `cancelled` and throw that report away.
    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Relaxed)
    }

    /// Entries found so far, from index `from` on. Each is complete when it appears, so a UI can
    /// list them while the scan is still running.
    pub fn entries_from(&self, from: usize) -> Vec<Entry> {
        self.entries.lock().map(|e| e.get(from..).unwrap_or_default().to_vec()).unwrap_or_default()
    }

    /// A snapshot that's safe to take from another thread while `run` is going.
    pub fn progress(&self) -> Progress {
        Progress {
            files: self.files.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            path: self.current.lock().map(|p| p.clone()).unwrap_or_default(),
            cats: self.cat_totals(),
        }
    }

    fn cat_totals(&self) -> BTreeMap<Cat, u64> {
        Cat::ALL.iter().map(|&c| (c, self.cats[c as usize].load(Relaxed))).collect()
    }

    fn roots(&self) -> Vec<Root> {
        let mut roots =
            vec![Root { path: self.home.clone(), rel: String::new(), cat: Cat::Other, label: "Home".into() }];
        if self.home_only {
            return roots;
        }
        let mut extra: Vec<(String, Cat, String)> =
            rules::SYSTEM_ROOTS.iter().map(|&(p, c, l)| (p.to_owned(), c, l.to_owned())).collect();
        if let Some(t) = &self.user_temp {
            extra.push((t.clone(), Cat::Caches, "Your temp folder".into()));
        }
        if let Some(c) = &self.user_cache {
            extra.push((c.clone(), Cat::Caches, "Your system caches".into()));
        }
        for (rel, cat, label) in extra {
            let path = PathBuf::from(&rel);
            // Skip roots inside home (an unusual home location) and ones on another volume.
            if path.starts_with(&self.home) || !fs::metadata(&path).is_ok_and(|m| m.is_dir() && m.dev() == self.dev) {
                continue;
            }
            roots.push(Root { path, rel, cat, label });
        }
        roots
    }

    pub fn run(&self) -> Report {
        let started = Instant::now();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(THREADS)
            .stack_size(16 << 20)
            .thread_name(|i| format!("scan-{i}"))
            .build()
            .ok();
        let job = || {
            self.ollama();
            let roots = self.roots();
            // simctl takes a moment to answer, so ask while the walk runs.
            let (mut nodes, runtimes): (Vec<Node>, Option<Node>) = rayon::join(
                || roots.par_iter().map(|r| self.walk_root(r)).collect(),
                || if self.home_only { None } else { self.sim_runtimes() },
            );
            nodes.extend(runtimes);
            nodes
        };
        let mut nodes = match &pool {
            Some(pool) => pool.install(job),
            None => job(),
        };
        let mut entries = self.entries.lock().unwrap().clone();
        entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.key.cmp(&b.key)));
        nodes.retain(|n| n.size > 0 || n.path.as_deref() == Some(""));

        let scanned: u64 = nodes.iter().map(|n| n.size).sum();
        let disk = sys::disk(&self.home);
        let unscanned = (disk.total - disk.free).saturating_sub(scanned);
        if !self.home_only && unscanned > 0 {
            nodes.push(Node {
                name: "macOS & everything else".into(),
                size: unscanned,
                cat: Cat::System,
                virt: true,
                ..Default::default()
            });
        }
        nodes.sort_by_key(|n| std::cmp::Reverse(n.size));
        let tree = Node {
            name: "This Mac".into(),
            size: nodes.iter().map(|n| n.size).sum(),
            children: nodes,
            ..Default::default()
        };

        Report {
            generated: sys::now(),
            seconds: (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            home: self.home.display().to_string(),
            computer: sys::computer_name(),
            disk,
            scanned,
            files: self.files.load(Relaxed),
            cats: self.cat_totals(),
            recs: rules::RECS,
            entries,
            unreadable: std::mem::take(&mut *self.unreadable.lock().unwrap()),
            unreadable_total: self.unreadable_total.load(Relaxed),
            full_access: self.home_only || sys::full_disk_access(),
            trash_readable: !self.trash_unreadable.load(Relaxed),
            tree,
        }
    }

    fn walk_root(&self, r: &Root) -> Node {
        let home = r.rel.is_empty();
        let ctx = Ctx {
            cat: (!home).then_some(r.cat),
            claimed: false,
            in_bundle: false,
            pat_ok: false,
            wt: None,
            // The home folder's children get special treatment at depth 0.
            depth: if home { 0 } else { 1 },
            tree_depth: 0,
            activity: 0,
            quiet: !home,
            mtime: 0,
        };
        let mut node = self.walk(&r.path, &r.rel, ctx, None, None);
        node.name.clone_from(&r.label);
        if home {
            // Home holds every category; colour it neutrally rather than by whatever dominates.
            node.cat = Cat::Other;
        }
        node.path = Some(r.rel.clone());
        node
    }

    fn push(&self, e: Entry) {
        self.entries.lock().unwrap().push(e);
    }

    fn note_unreadable(&self, rel: String) {
        self.unreadable_total.fetch_add(1, Relaxed);
        if rel == ".Trash" {
            self.trash_unreadable.store(true, Relaxed);
        }
        let mut u = self.unreadable.lock().unwrap();
        if u.len() < MAX_UNREADABLE {
            u.push(rel);
        }
    }

    fn count(&self, cat: Cat, size: u64) {
        self.bytes.fetch_add(size, Relaxed);
        self.cats[cat as usize].fetch_add(size, Relaxed);
    }

    fn idle(&self, activity: i64) -> Option<u32> {
        (activity > 0).then(|| ((self.now - activity).max(0) / DAY) as u32)
    }

    /// Whether a cache folder named `name` (an app's name or bundle identifier) belongs to a running app.
    fn live(&self) -> &Live {
        self.live.get_or_init(|| {
            let handle = self.probe.lock().ok().and_then(|mut p| p.take());
            handle.and_then(|h| h.join().ok()).unwrap_or_default()
        })
    }

    fn is_running(&self, name: &str) -> bool {
        app_is_running(&self.live().running, name)
    }

    fn app_hold(&self, name: &str) -> Option<String> {
        self.is_running(name).then(|| "app is running".to_owned())
    }

    fn owned(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| m.uid() == self.uid)
    }

    // ---- walking -----------------------------------------------------------------------

    /// `ident` is the folder's device and inode as its parent listed it.
    fn walk(&self, path: &Path, rel: &str, mut ctx: Ctx, mut hit: Option<Hit>, ident: Option<(u64, u64)>) -> Node {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if self.cancelled() {
            return Node { name, ..Default::default() };
        }
        if rules::CLOUD_ROOTS.contains(&rel) {
            return self.cloud(path, rel, name, ctx.cat.unwrap_or(Cat::Other));
        }
        // Scan roots may be symlinks (/tmp); below them only real folders are entered.
        let items: Vec<Item> = match bulk::list(path, ctx.tree_depth == 0) {
            Ok(items) => items,
            Err(_) => {
                if !ctx.quiet {
                    self.note_unreadable(rel.to_owned());
                }
                return Node { name, cat: ctx.cat.unwrap_or(Cat::Other), unreadable: true, ..Default::default() };
            }
        };
        if ctx.depth <= 4
            && let Ok(mut c) = self.current.try_lock()
        {
            rel.clone_into(&mut c);
        }
        let names: HashSet<&str> = items.iter().map(|i| i.name.as_str()).collect();
        let is_project = rules::PROJECT_MARKERS.iter().any(|m| names.contains(m));

        // A linked git worktree: everything inside counts as the worktree, and its build output and
        // dependencies can still be cleared on their own.
        let mut own_wt = false;
        if hit.is_none()
            && !ctx.claimed
            && ctx.wt.is_none()
            && ctx.depth > 0
            && items.iter().any(|i| i.name == ".git" && i.kind == Kind::File)
            && let Some(gitdir) = git::linked_gitdir(&path.join(".git"))
        {
            hit = Some(Hit { gitdir: Some(gitdir), ..Hit::new("worktrees") });
            ctx.cat = Some(Cat::Worktrees);
            ctx.pat_ok = true;
            ctx.wt = Some(Arc::from(rules::key("worktrees", rel)));
            own_wt = true;
        }
        if ctx.depth > 0 && matches!(ctx.cat, None | Some(Cat::Other)) && !ctx.in_bundle && is_project {
            ctx.cat = Some(Cat::Code);
        }
        if hit.is_none() && !ctx.claimed && ctx.pat_ok && names.contains("pyvenv.cfg") {
            hit = Some(Hit { idle_days: self.idle(ctx.activity), ..Hit::new("venv").label(rel) });
            if ctx.cat != Some(Cat::Worktrees) {
                ctx.cat = Some(Cat::Deps);
            }
        }
        let claim_kids = ctx.claimed || hit.as_ref().is_some_and(|h| h.rec != "worktrees");
        let activity = if !claim_kids && is_project { self.activity(path, &items, hit.as_ref()) } else { ctx.activity };

        let walk_kids = || -> Vec<Node> {
            items
                .par_iter()
                .map(|item| {
                    let cname = &item.name;
                    let crel = if rel.is_empty() { cname.clone() } else { format!("{rel}/{cname}") };
                    if item.kind == Kind::Dir {
                        if item.dev != self.dev {
                            return Node::default();
                        }
                        // Nothing inside an app bundle or a repository's own storage is offered.
                        let bundle = ctx.in_bundle || rules::is_bundle(cname) || rules::is_vcs(cname);
                        let chit = if claim_kids {
                            None
                        } else {
                            self.match_dir(&crel, rel, cname, &names, ctx.pat_ok && !bundle, activity)
                        };
                        let ccat = rules::child_cat(ctx.cat, chit.as_ref().map(|h| h.rec), &crel);
                        let pat_ok = if ctx.depth > 0 {
                            ctx.pat_ok
                        } else {
                            !(cname.starts_with('.') || cname == "Library" || cname == "Applications")
                        };
                        let kid = Ctx {
                            cat: ccat,
                            claimed: claim_kids,
                            in_bundle: bundle,
                            pat_ok,
                            wt: ctx.wt.clone(),
                            depth: ctx.depth + 1,
                            tree_depth: ctx.tree_depth + 1,
                            activity,
                            quiet: ctx.quiet,
                            mtime: item.mtime,
                        };
                        self.walk(&path.join(cname), &crel, kid, chit, Some((item.dev, item.ino)))
                    } else if item.kind == Kind::File {
                        if item.nlink > 1 && !self.seen.lock().unwrap().insert((item.dev, item.ino)) {
                            return Node::default();
                        }
                        let size = item.size;
                        let cat = ctx.cat.unwrap_or(Cat::Other);
                        self.files.fetch_add(1, Relaxed);
                        self.count(cat, size);
                        if !claim_kids {
                            self.match_file(path, &crel, rel, item, &ctx, cat);
                        }
                        // Only kept nodes need a name.
                        let name = if size >= MIN_NODE { cname.clone() } else { String::new() };
                        Node { name, size, cat, file: true, newest: item.mtime, ..Default::default() }
                    } else {
                        Node::default()
                    }
                })
                .collect()
        };
        // git is asked about a worktree while its files are being counted, not after the whole scan.
        let (kids, inspected) = match hit.as_ref().and_then(|h| h.gitdir.as_deref()) {
            Some(gitdir) => {
                let inspect =
                    || if self.cancelled() { Worktree::default() } else { git::inspect(path, rel, gitdir, &self.home) };
                let (kids, mut info) = rayon::join(walk_kids, inspect);
                info.busy = self.live().cwds.as_ref().is_some_and(|c| c.iter().any(|cwd| cwd.starts_with(path)));
                (kids, Some(info))
            }
            None => (walk_kids(), None),
        };

        let total: u64 = kids.iter().map(|k| k.size).sum();
        // A folder's own time counts too: one unpacked today from an archive of old files was
        // touched today, and cleanup's "still untouched?" check looks at folders as well.
        let newest = kids.iter().map(|k| k.newest).max().unwrap_or(0).max(ctx.mtime);
        let cat = ctx.cat.unwrap_or_else(|| dominant(&kids));

        if let Some(h) = hit {
            let fresh = h.min_age > 0 && self.now - newest < h.min_age;
            // A spared child stays behind, so it isn't part of what clearing the entry frees.
            let spared =
                h.spare.and_then(|s| items.iter().zip(&kids).find(|(i, _)| i.name == s)).map_or(0, |(_, k)| k.size);
            let total = total - spared;
            if total >= MIN_ENTRY.max(h.min_size) && !fresh {
                let idle_days = if h.min_age > 0 { self.idle(newest) } else { h.idle_days };
                self.push(Entry {
                    key: rules::key(h.rec, rel),
                    rec: h.rec,
                    path: rel.to_owned(),
                    size: total,
                    cat: ctx.cat.unwrap_or(Cat::Other),
                    label: h.label,
                    idle_days,
                    mtime: h.mtime,
                    cmd: h.cmd,
                    hold: h.hold,
                    // Kept whatever it weighed when scanned: it may be created or filled before the cleanup.
                    spare: h.spare.map(str::to_owned),
                    parent: if own_wt { None } else { ctx.wt.as_deref().map(str::to_owned) },
                    worktree: inspected,
                    abs: path.to_path_buf(),
                    gitdir: h.gitdir,
                    ident,
                    scanned: self.now,
                    min_age: h.min_age,
                });
            }
        }
        let children = if ctx.tree_depth < MAX_TREE_DEPTH && !ctx.in_bundle { prune(kids) } else { Vec::new() };
        Node { name, size: total, cat, children, newest, ..Default::default() }
    }

    /// When a project was last touched: its newest top-level item (ignoring build output and
    /// dependencies) or its git index, whichever is later.
    fn activity(&self, path: &Path, items: &[Item], hit: Option<&Hit>) -> i64 {
        let newest = items
            .iter()
            .filter(|i| !rules::BYPRODUCTS.contains(&i.name.as_str()) && !rules::BUILD_DIRS.contains(&i.name.as_str()))
            .map(|i| i.mtime)
            .max()
            .unwrap_or(0);
        let gitdir = match hit.and_then(|h| h.gitdir.clone()) {
            Some(g) => g,
            None => path.join(".git"),
        };
        newest.max(sys::mtime(&gitdir.join("index")))
    }

    // ---- matching ----------------------------------------------------------------------

    fn match_dir(
        &self,
        crel: &str,
        prel: &str,
        name: &str,
        siblings: &HashSet<&str>,
        pat_ok: bool,
        activity: i64,
    ) -> Option<Hit> {
        if crel.starts_with('/') {
            return self.match_outside(crel, prel, name);
        }
        if rules::SKIP.contains(crel) {
            return None;
        }
        if let Some(&id) = rules::FIXED.get(crel) {
            return Some(Hit::new(id));
        }
        if let Some(&id) = rules::CHILDREN_OF.get(prel) {
            return self.child_hit(id, crel, prel, name);
        }
        let (pp, pname) = prel.rsplit_once('/').unwrap_or(("", prel));
        if let Some(&id) = rules::GRANDCHILDREN_OF.get(pp) {
            let label =
                if pp == ".codex/sessions" { format!("Codex · {pname}-{name}") } else { format!("{pname}/{name}") };
            return Some(Hit { mtime: Some(sys::mtime(&self.home.join(crel))), ..Hit::new(id).label(label) });
        }
        if let Some(app) = crel.strip_prefix("Library/Containers/").and_then(|r| r.strip_suffix("/Data/Library/Caches"))
            && !app.contains('/')
            && !rules::APPLE_CACHES.iter().any(|a| app.starts_with(a))
        {
            return Some(Hit { hold: self.app_hold(app), ..Hit::new("containercaches").label(app) });
        }
        // Only where agent CLIs install themselves: a deploy folder laid out the same way
        // (`releases/` beside a `current` link) holds real builds and uploads.
        if pname == "releases" && rules::AGENT_PACKAGES.iter().any(|a| pp.starts_with(a)) {
            let current = self.home.join(pp).join("current");
            let is_link = fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink());
            if is_link && fs::canonicalize(&current).ok() != fs::canonicalize(self.home.join(crel)).ok() {
                let tool = pp.rsplit('/').next().unwrap_or(pp);
                return Some(Hit::new("oldversions").label(format!("{tool}: {name}")));
            }
        }
        if let Some(rest) = prel.strip_prefix("Library/Application Support/")
            && rules::ELECTRON_CACHE.contains(&name)
            && !rest.starts_with("MobileSync")
        {
            let app = rest.split('/').next().unwrap_or(rest);
            return Some(Hit { hold: self.app_hold(app), ..Hit::new("electron").label(format!("{app}: {name}")) });
        }
        if pat_ok {
            if name == "node_modules" && siblings.contains("package.json") {
                return Some(Hit { idle_days: self.idle(activity), ..Hit::new("node_modules").label(prel) });
            }
            if rules::BUILD_DIRS.contains(&name)
                || rules::build_with(name, siblings.iter().copied())
                || rules::BUILD_INSIDE.contains(&(pname, name))
            {
                return Some(Hit { idle_days: self.idle(activity), ..Hit::new("build").label(crel) });
            }
        }
        None
    }

    /// Rules for folders outside the home folder. Only items the user owns are offered.
    fn match_outside(&self, crel: &str, prel: &str, name: &str) -> Option<Hit> {
        let claude_tmp = format!("/private/tmp/claude-{}", self.uid);
        if prel == "/Applications" {
            // Xcode.app, Xcode-beta.app, Xcode_26.1.app, "Xcode 26.app"; not Xcodes.app or other tools.
            let is_xcode = name
                .strip_suffix(".app")
                .and_then(|n| n.strip_prefix("Xcode"))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_', ' ']));
            let selected = self.xcode.as_deref().is_some_and(|x| fs::canonicalize(crel).is_ok_and(|c| c == x));
            return (is_xcode && !selected && self.xcode.is_some() && self.owned(Path::new(crel)))
                .then(|| Hit::new("oldxcode").label(name));
        }
        for cellar in ["/opt/homebrew/Cellar", "/usr/local/Cellar"] {
            if let Some(formula) = prel.strip_prefix(cellar).and_then(|r| r.strip_prefix('/'))
                && !formula.contains('/')
            {
                let opt = Path::new(cellar).with_file_name("opt").join(formula);
                let linked = fs::canonicalize(&opt).ok();
                if linked.is_some() && linked != fs::canonicalize(crel).ok() {
                    let cmd = ["brew", "cleanup", formula].map(String::from).to_vec();
                    return Some(Hit { cmd: Some(cmd), ..Hit::new("brewold").label(format!("{formula} {name}")) });
                }
                return None;
            }
        }
        if !self.owned(Path::new(crel)) {
            return None;
        }
        if prel == claude_tmp {
            let project = name.trim_start_matches(&self.home.display().to_string().replace(['/', '.'], "-"));
            let project = project.trim_start_matches('-');
            let label = if project.is_empty() { "Claude Code".to_owned() } else { format!("Claude Code · {project}") };
            return Some(Hit { min_age: DAY, ..Hit::new("agenttmp").label(label) });
        }
        if prel == "/private/tmp" && crel != claude_tmp {
            return Some(Hit { min_age: DAY, ..Hit::new("tmpfiles").label(name) });
        }
        let apple = rules::APPLE_CACHES.iter().any(|a| name.starts_with(a));
        if Some(prel) == self.user_temp.as_deref() && !apple {
            return Some(Hit { min_age: DAY, ..Hit::new("tmpfiles").label(format!("Temp · {name}")) });
        }
        if Some(prel) == self.user_cache.as_deref() && !apple {
            return Some(Hit { hold: self.app_hold(name), ..Hit::new("syscaches").label(name) });
        }
        None
    }

    fn child_hit(&self, id: &'static str, crel: &str, prel: &str, name: &str) -> Option<Hit> {
        let full = self.home.join(crel);
        let hit = Hit::new(id);
        match id {
            "appcaches" if rules::APPLE_CACHES.iter().any(|a| name.starts_with(a)) => None,
            "appcaches" => Some(Hit { hold: self.app_hold(name), ..hit.label(name) }),
            "editorstate" => {
                let folder = missing_editor_folder(&full)?;
                let app = prel.strip_prefix("Library/Application Support/")?.split('/').next()?;
                let shown = match Path::new(&folder).strip_prefix(&self.home) {
                    Ok(r) => format!("~/{}", r.display()),
                    Err(_) => folder,
                };
                Some(Hit { mtime: Some(sys::mtime(&full)), ..hit.label(format!("{app} · {shown}")) })
            }
            "downloads" => Some(Hit { min_age: OLD_DOWNLOAD, min_size: DOWNLOAD_MIN, ..hit.label(name) }),
            "iosbackups" => {
                let info: plist::Dictionary = plist::from_file(full.join("Info.plist")).unwrap_or_default();
                let text = |key: &str| info.get(key).and_then(|v| v.as_string()).map(str::to_owned);
                let label = match (text("Device Name"), text("Product Name")) {
                    (Some(device), Some(model)) => format!("{device} ({model})"),
                    (Some(device), None) => device,
                    _ => format!("Backup {}", &name[..name.len().min(8)]),
                };
                Some(Hit { mtime: Some(sys::mtime(&full)), ..hit.label(label) })
            }
            "simulators" => {
                if !is_uuid(name) {
                    return None;
                }
                let dev: plist::Dictionary = plist::from_file(full.join("device.plist")).unwrap_or_default();
                let device = dev.get("name").and_then(|v| v.as_string()).unwrap_or(name);
                let runtime = dev
                    .get("runtime")
                    .and_then(|v| v.as_string())
                    .and_then(|r| r.rsplit('.').next())
                    .map(|r| r.replacen('-', " ", 1).replace('-', "."));
                let label = match runtime {
                    Some(r) => format!("{device} ({r})"),
                    None => device.to_owned(),
                };
                let booted = crate::clean::simulator_last_used(&dev);
                // 3 is booted. It is in use whatever its last-used date says.
                let booted_now = dev.get("state").and_then(|v| v.as_unsigned_integer()) == Some(3);
                Some(Hit {
                    cmd: Some(vec!["xcrun".into(), "simctl".into(), "delete".into(), name.into()]),
                    idle_days: booted.and_then(|t| self.idle(t)),
                    hold: booted_now.then(|| "booted".to_owned()),
                    ..hit.label(label)
                })
            }
            "toolchains" => {
                let lang = rules::TOOLCHAIN_NAMES.iter().find(|(p, _)| *p == prel).map_or("", |(_, l)| l);
                let hold = self.defaults.contains(crel).then(|| "default version".to_owned());
                Some(Hit { hold, ..hit.label(format!("{lang} {name}")) })
            }
            "android" => Some(hit.label(match name.strip_suffix(".avd") {
                Some(avd) => format!("Emulator {avd}"),
                None if prel.ends_with("/ndk") => format!("NDK {name}"),
                None => format!("System image {name}"),
            })),
            "agentlogs" => {
                let claude = prel == ".claude/projects";
                let label = if claude {
                    format!("Claude Code · {}", self.claude_project(&full, name))
                } else {
                    format!("Gemini · {}", &name[..name.len().min(12)])
                };
                // Claude Code keeps what it has learned about a project next to the transcripts.
                Some(Hit { mtime: Some(sys::mtime(&full)), spare: claude.then_some("memory"), ..hit.label(label) })
            }
            "oldversions" => {
                let current = self.claude_current.as_deref();
                let this = fs::canonicalize(&full).ok();
                let in_use = current.is_some_and(|c| this.as_deref().is_some_and(|t| c.starts_with(t)));
                (!in_use).then(|| hit.label(format!("claude: {name}")))
            }
            _ => Some(hit.label(name)),
        }
    }

    /// Claude Code names project folders after the path with `/` and `.` replaced by `-`, which
    /// can't be reversed reliably, so read the `cwd` from a transcript instead.
    fn claude_project(&self, dir: &Path, name: &str) -> String {
        let cwd = fs::read_dir(dir).ok().and_then(|rd| {
            rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "jsonl")).find_map(|e| {
                use std::io::Read;
                let mut buf = vec![0; 64 * 1024];
                let n = fs::File::open(e.path()).and_then(|mut f| f.read(&mut buf)).ok()?;
                let text = String::from_utf8_lossy(&buf[..n]);
                let start = text.find("\"cwd\":\"")? + 7;
                let end = text[start..].find('"')? + start;
                Some(text[start..end].to_owned())
            })
        });
        match cwd {
            Some(cwd) => match Path::new(&cwd).strip_prefix(&self.home) {
                Ok(r) if r.as_os_str().is_empty() => "~".into(),
                Ok(r) => format!("~/{}", r.display()),
                Err(_) => cwd,
            },
            None => name.to_owned(),
        }
    }

    fn match_file(&self, dir: &Path, crel: &str, prel: &str, item: &Item, ctx: &Ctx, cat: Cat) {
        let (name, size) = (item.name.as_str(), item.size);
        let entry =
            |rec: &'static str, label: Option<String>, cmd: Option<Vec<String>>, idle_days: Option<u32>| Entry {
                key: rules::key(rec, crel),
                rec,
                path: crel.to_owned(),
                size,
                cat,
                label,
                idle_days,
                mtime: Some(item.mtime),
                cmd,
                abs: dir.join(name),
                ident: Some((item.dev, item.ino)),
                scanned: self.now,
                min_age: match rec {
                    "tmpfiles" => DAY,
                    "downloads" => OLD_DOWNLOAD,
                    _ => 0,
                },
                ..Default::default()
            };
        if let Some(&id) = rules::FIXED_FILES.get(crel) {
            self.push(entry(id, None, rules::command(id), None));
            return;
        }
        if crel.starts_with('/') {
            // Loose files in /tmp, like disk images and archives tools stage there.
            let label = if prel == "/private/tmp" {
                Some(name.to_owned())
            } else if Some(prel) == self.user_temp.as_deref() {
                Some(format!("Temp · {name}"))
            } else {
                None
            };
            let apple = rules::APPLE_CACHES.iter().any(|a| name.starts_with(a));
            if let Some(label) = label
                && !apple
                && size >= MIN_ENTRY
                && self.now - item.mtime >= DAY
                && item.uid == self.uid
            {
                self.push(entry("tmpfiles", Some(label), None, self.idle(item.mtime)));
            }
            return;
        }
        // The native Claude Code installer keeps each version as a single binary.
        if let Some(version) = crel.strip_prefix(".local/share/claude/versions/").filter(|v| !v.contains('/')) {
            let in_use =
                self.claude_current.as_deref().is_some_and(|c| fs::canonicalize(dir.join(name)).is_ok_and(|t| t == c));
            if !in_use && size >= MIN_ENTRY {
                self.push(entry("oldversions", Some(format!("claude: {version}")), None, None));
            }
            return;
        }
        let top = crel.split('/').next().unwrap_or(crel);
        if ctx.in_bundle || ctx.wt.is_some() || top.starts_with('.') || top == "Library" || top == "Applications" {
            return;
        }
        let ext = rules::ext(name);
        let rec = if ext.as_deref().is_some_and(|e| rules::INSTALLER_EXT.contains(&e)) && size >= INSTALLER_MIN {
            "installers"
        } else if size >= LARGE_FILE || (top == "Downloads" && size >= LARGE_DOWNLOAD) {
            "largefiles"
        } else if prel == "Downloads" && size >= DOWNLOAD_MIN && self.now - item.mtime >= OLD_DOWNLOAD {
            "downloads"
        } else {
            return;
        };
        let idle = (rec == "downloads").then(|| self.idle(item.mtime)).flatten();
        self.push(entry(rec, None, None, idle));
    }

    // ---- special folders ---------------------------------------------------------------

    fn cloud(&self, path: &Path, rel: &str, name: String, cat: Cat) -> Node {
        let names: Vec<String> = match fs::read_dir(path) {
            Ok(rd) => rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => {
                self.note_unreadable(rel.to_owned());
                return Node { name, cat, unreadable: true, ..Default::default() };
            }
        };
        let deadline = Instant::now() + CLOUD_BUDGET;
        let kids: Vec<Node> = names
            .par_iter()
            .map(|child| {
                let left = deadline.saturating_duration_since(Instant::now());
                let size = (left > Duration::from_secs(1))
                    .then(|| {
                        let mut cmd = Command::new("du");
                        cmd.arg("-sk").arg(path.join(child));
                        sys::run(cmd, Some(CLOUD_TIMEOUT.min(left)))
                    })
                    .flatten()
                    // A partial answer (some folders unreadable) would look like a complete one.
                    .filter(|o| o.ok)
                    .and_then(|o| o.stdout.split_whitespace().next()?.parse::<u64>().ok())
                    .map(|kb| kb * 1024);
                match size {
                    Some(s) => {
                        self.count(cat, s);
                        Node { name: child.clone(), size: s, cat, ..Default::default() }
                    }
                    None => {
                        self.note_unreadable(format!("{rel}/{child} (iCloud didn't respond)"));
                        Node { name: child.clone(), cat, unreadable: true, ..Default::default() }
                    }
                }
            })
            .collect();
        Node { name, size: kids.iter().map(|k| k.size).sum(), cat, children: prune(kids), ..Default::default() }
    }

    /// Simulator runtimes live in system-managed disk images that can't be walked, but `simctl`
    /// reports each one's size. Only asked when a full Xcode is selected, so no install prompt appears.
    fn sim_runtimes(&self) -> Option<Node> {
        let simctl = self.xcode.as_ref()?.join("Contents/Developer/usr/bin/simctl");
        if !simctl.exists() {
            return None;
        }
        if let Ok(mut c) = self.current.lock() {
            "Asking simctl about simulator runtimes".clone_into(&mut c);
        }
        let mut cmd = Command::new(simctl);
        cmd.args(["runtime", "list", "-j"]);
        let out = sys::run(cmd, Some(Duration::from_secs(30))).filter(|o| o.ok)?;
        let list: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&out.stdout).ok()?;
        let mut kids = Vec::new();
        for rt in list.values() {
            let size: u64 = match &rt["sizeBytes"] {
                serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
                serde_json::Value::String(s) => s.parse().unwrap_or(0),
                _ => 0,
            };
            let (Some(id), true) = (rt["identifier"].as_str(), size > 0) else { continue };
            let platform = rt["platformIdentifier"].as_str().unwrap_or("").rsplit('.').next().unwrap_or("");
            let os = match platform {
                "iphonesimulator" => "iOS",
                "watchsimulator" => "watchOS",
                "appletvsimulator" => "tvOS",
                "xrsimulator" => "visionOS",
                _ => "Simulator",
            };
            let label = format!("{os} {}", rt["version"].as_str().unwrap_or(""));
            let path = rt["path"].as_str().unwrap_or("/System/Library/AssetsV2").to_owned();
            self.count(Cat::Toolchains, size);
            // Only offered when simctl says it can be deleted.
            if rt["deletable"].as_bool() == Some(true) {
                let last_used = rt["lastUsedAt"].as_str().and_then(parse_iso_date);
                self.push(Entry {
                    key: rules::key("simruntimes", id),
                    rec: "simruntimes",
                    path: path.clone(),
                    size,
                    cat: Cat::Toolchains,
                    label: Some(label.clone()),
                    idle_days: last_used.and_then(|t| self.idle(t)),
                    mtime: None,
                    cmd: Some(["xcrun", "simctl", "runtime", "delete", id].map(String::from).to_vec()),
                    abs: PathBuf::from(path),
                    scanned: self.now,
                    ..Default::default()
                });
            }
            kids.push(Node { name: label, size, cat: Cat::Toolchains, file: true, virt: true, ..Default::default() });
        }
        (!kids.is_empty()).then(|| Node {
            name: "Simulator runtimes".into(),
            size: kids.iter().map(|k| k.size).sum(),
            cat: Cat::Toolchains,
            children: prune(kids),
            virt: true,
            ..Default::default()
        })
    }

    /// Ollama stores models as content-addressed blobs; the manifests say which model each belongs to.
    fn ollama(&self) {
        let mdir = self.home.join(".ollama/models/manifests");
        let mut stack = vec![mdir.clone()];
        // (name, [(digest, size)]) per model tag.
        let mut models: Vec<(String, Vec<(String, u64)>)> = Vec::new();
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(p);
                    continue;
                }
                let Ok(rel) = p.strip_prefix(&mdir) else { continue };
                let parts: Vec<String> = rel.iter().map(|s| s.to_string_lossy().into_owned()).collect();
                let [host, ns, model, tag] = parts.as_slice() else { continue };
                let name = match (host.as_str(), ns.as_str()) {
                    ("registry.ollama.ai", "library") => format!("{model}:{tag}"),
                    ("registry.ollama.ai", _) => format!("{ns}/{model}:{tag}"),
                    _ => format!("{host}/{ns}/{model}:{tag}"),
                };
                let Ok(m) = fs::read(&p)
                    .map_err(drop)
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).map_err(drop))
                else {
                    continue;
                };
                let layers = m["layers"].as_array().cloned().unwrap_or_default();
                let layers = layers
                    .iter()
                    .map(|l| (l["digest"].as_str().unwrap_or("").to_owned(), l["size"].as_u64().unwrap_or(0)));
                models.push((name, layers.collect()));
            }
        }
        models.sort();
        // Tags of the same model share blobs, and `ollama rm` only frees the ones nothing else uses.
        let mut uses: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (_, layers) in &models {
            for digest in layers.iter().map(|(d, _)| d.as_str()).collect::<HashSet<_>>() {
                *uses.entry(digest).or_default() += 1;
            }
        }
        // Tags with the same weights (an alias, or a model built FROM another) free them only when
        // every one of them goes. Offered apart, each would weigh next to nothing and removing one
        // would free nothing, so they are one entry, removed together.
        let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, (_, layers)) in models.iter().enumerate() {
            if let Some((weights, _)) = layers.iter().max_by_key(|(_, size)| *size) {
                groups.entry(weights).or_default().push(i);
            }
        }
        for (weights, tags) in groups {
            let tags: Vec<_> = tags.into_iter().map(|i| &models[i]).collect();
            let names: Vec<String> = tags.iter().map(|(name, _)| name.clone()).collect();
            // What removing all of them frees: the blobs no tag outside the group uses.
            let mut inside: std::collections::HashMap<&str, (usize, u64)> = std::collections::HashMap::new();
            for (_, layers) in &tags {
                for digest in layers.iter().map(|(d, _)| d.as_str()).collect::<HashSet<_>>() {
                    let size = layers.iter().find(|(d, _)| d == digest).map_or(0, |(_, size)| *size);
                    inside.entry(digest).or_insert((0, size)).0 += 1;
                }
            }
            let own: u64 = inside.iter().filter(|(d, (n, _))| uses[*d] == *n).map(|(_, (_, size))| size).sum();
            let blob = format!(".ollama/models/blobs/{}", weights.replace(':', "-"));
            let label = names.join(", ");
            self.push(Entry {
                key: rules::key("ollama", &label),
                rec: "ollama",
                path: blob.clone(),
                size: own,
                cat: Cat::Models,
                label: Some(label),
                cmd: Some(["ollama", "rm"].into_iter().map(String::from).chain(names).collect()),
                abs: self.home.join(blob),
                scanned: self.now,
                ..Default::default()
            });
        }
    }
}

/// Whether a cache folder named `name` (an app's name or bundle identifier) belongs to one of the
/// `running` apps (lowercased names and identifiers, from `sys::running_apps`).
pub fn app_is_running(running: &HashSet<String>, name: &str) -> bool {
    let name = name.to_lowercase();
    let alias = APP_ALIASES.iter().find(|(dir, _)| *dir == name).map(|(_, id)| *id);
    // Helpers keep caches under the app's identifier plus a suffix (`com.app.id.ShipIt`).
    running.iter().any(|app| {
        name == *app || alias == Some(app) || name.strip_prefix(app.as_str()).is_some_and(|r| r.starts_with('.'))
    })
}

/// Folders of the language versions set as default, home-relative. Removing one of these breaks
/// every shell, and with nvm the global CLIs installed under it.
pub fn default_toolchains(home: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let read = |rel: &str| fs::read_to_string(home.join(rel)).unwrap_or_default();
    for line in read(".rustup/settings.toml").lines() {
        if let Some(v) = line.trim().strip_prefix("default_toolchain").and_then(|r| r.split('"').nth(1)) {
            out.insert(format!(".rustup/toolchains/{v}"));
        }
    }
    for version in read(".pyenv/version").split_whitespace() {
        out.insert(format!(".pyenv/versions/{version}"));
    }
    // nvm's default is an alias, and aliases can point at aliases: `default` -> `lts/*` -> `lts/jod` -> `v22.12.0`.
    let installed: Vec<String> = fs::read_dir(home.join(".nvm/versions/node"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let key = |v: &str| v.trim_start_matches('v').split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let mut alias = read(".nvm/alias/default").trim().to_owned();
    let numeric = |a: &str| a.trim_start_matches('v').starts_with(|c: char| c.is_ascii_digit());
    for _ in 0..4 {
        if alias.is_empty() || numeric(&alias) || matches!(alias.as_str(), "node" | "stable" | "system") {
            break;
        }
        alias = read(&format!(".nvm/alias/{alias}")).trim().to_owned();
    }
    let held: Vec<&String> = match alias.as_str() {
        // nvm isn't set up, or the default is whatever the system has.
        "" | "system" if !home.join(".nvm/alias/default").exists() || alias == "system" => Vec::new(),
        // `22`, `v22` or `22.3.0`: the newest installed version it matches.
        a if numeric(a) => {
            let want = a.trim_start_matches('v');
            let matching = installed.iter().filter(|v| {
                let v = v.trim_start_matches('v');
                v == want || v.starts_with(&format!("{want}."))
            });
            matching.max_by_key(|v| key(v)).into_iter().collect()
        }
        "node" | "stable" => installed.iter().max_by_key(|v| key(v)).into_iter().collect(),
        // An alias that leads nowhere this can read: any of them might be the default, so hold them all.
        _ => installed.iter().collect(),
    };
    out.extend(held.into_iter().map(|v| format!(".nvm/versions/node/{v}")));
    out
}

/// The folder an editor's `workspaceStorage/<id>` state belongs to, as its `workspace.json` names
/// it, if that folder is gone. A folder on another volume may only be unmounted, so it never counts
/// as gone, and neither does one that can't be read from the record. Cleanup asks again, since the
/// folder can come back after the scan and the editor would pick its state up again.
pub fn missing_editor_folder(dir: &Path) -> Option<String> {
    let ws: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("workspace.json")).ok()?).ok()?;
    let folder = percent_decode(ws["folder"].as_str()?.strip_prefix("file://")?);
    (!folder.starts_with("/Volumes/") && !Path::new(&folder).exists()).then_some(folder)
}

/// Decodes `%20`-style escapes in a `file://` URL's path.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex =
            (bytes[i] == b'%').then(|| s.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Keeps the largest children and folds the rest into one "N smaller items" node.
fn prune(mut kids: Vec<Node>) -> Vec<Node> {
    kids.sort_by_key(|k| std::cmp::Reverse(k.size));
    let keep = kids.iter().take(MAX_CHILDREN).take_while(|k| k.size >= MIN_NODE).count();
    if keep == 0 {
        return Vec::new();
    }
    let rest = kids.split_off(keep);
    let rest_size: u64 = rest.iter().map(|k| k.size).sum();
    if rest_size > 0 {
        kids.push(Node {
            name: format!("{} smaller items", rest.len()),
            size: rest_size,
            cat: dominant(&rest),
            others: rest.len() as u32,
            ..Default::default()
        });
    }
    kids
}

/// The category holding the most bytes among `kids`.
fn dominant(kids: &[Node]) -> Cat {
    let mut by = [0u64; Cat::COUNT];
    for k in kids {
        by[k.cat as usize] += k.size;
    }
    Cat::ALL.into_iter().max_by_key(|c| by[*c as usize]).unwrap_or_default()
}

fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|g| g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Unix seconds for an ISO 8601 UTC timestamp like `2026-11-12T21:11:36Z`.
fn parse_iso_date(s: &str) -> Option<i64> {
    let (date, time) = s.trim_end_matches('Z').split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|x| x.split('.').next().and_then(|x| x.parse::<i64>().ok()));
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next().flatten().unwrap_or(0));
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * DAY + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_are_matched_to_running_apps() {
        let running: HashSet<String> =
            ["slack", "com.tinyspeck.slackmacgap", "com.google.chrome", "com.microsoft.vscode"]
                .map(String::from)
                .into();
        assert!(app_is_running(&running, "Slack"));
        assert!(
            app_is_running(&running, "com.tinyspeck.slackmacgap.ShipIt"),
            "a helper's cache under the app's identifier"
        );
        assert!(
            app_is_running(&running, "Google") && app_is_running(&running, "Code"),
            "folders not named after their app"
        );
        assert!(!app_is_running(&running, "com.tinyspeck.slackmacgapother") && !app_is_running(&running, "Zed"));
    }

    #[test]
    fn iso_dates() {
        assert_eq!(parse_iso_date("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_date("2026-11-12T21:11:36Z"), Some(1_794_517_896));
    }

    #[test]
    fn prune_folds_small_items() {
        let n = |s, cat| Node { size: s, cat, ..Default::default() };
        let kids = prune(vec![n(9_000_000, Cat::Code), n(10, Cat::Caches), n(20, Cat::Caches)]);
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[1].others, 2);
        assert_eq!(kids[1].size, 30);
        assert_eq!(kids[1].cat, Cat::Caches);
        assert!(prune(vec![n(10, Cat::Code)]).is_empty(), "nothing big enough means a leaf");
    }
}
