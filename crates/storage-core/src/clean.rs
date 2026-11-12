//! Acting on cleanup entries.
//!
//! How an entry goes depends on its recommendation's tier:
//! - `Safe` (regenerated automatically): deleted outright.
//! - `Review` (the user's call): moved to the Trash, so it can be put back.
//! - Entries owned by a tool (simulators, Ollama models, Docker) run that tool's own command.
//! - The Trash itself is emptied.
//!
//! Every path is checked against the home folder first. Only entries produced by a scan are
//! ever acted on; callers look them up by key rather than passing paths in.

use crate::git;
use crate::rules::{self, Tier};
use crate::scan::Entry;
use crate::sys;
use serde::Serialize;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// Deleted permanently.
    Delete,
    /// Moved to the Trash.
    Trash,
    /// Removed by the owning tool's command.
    Command,
    /// The Trash was emptied.
    Empty,
}

pub fn method(e: &Entry) -> Method {
    if e.rec == "trash" {
        Method::Empty
    } else if e.cmd.is_some() {
        Method::Command
    } else {
        match rules::rec(e.rec).tier {
            Tier::Safe => Method::Delete,
            Tier::Review => Method::Trash,
        }
    }
}

/// Top-level folders that are never removed as a whole, whatever a rule says.
const PROTECTED: &[&str] = &[
    "Library",
    "Documents",
    "Desktop",
    "Downloads",
    "Developer",
    "Pictures",
    "Movies",
    "Music",
    "Applications",
    "Public",
    ".ssh",
    ".gnupg",
    ".config",
];

/// Folders outside the home folder where entries may be removed, and how deep. `/Applications` only
/// allows a whole app; the temp and cache folders allow anything inside them.
static OUTSIDE: LazyLock<Vec<(PathBuf, Option<usize>)>> = LazyLock::new(|| {
    let mut roots = vec![(PathBuf::from("/Applications"), Some(1)), (PathBuf::from("/private/tmp"), None)];
    for name in ["DARWIN_USER_TEMP_DIR", "DARWIN_USER_CACHE_DIR"] {
        if let Some(dir) = sys::darwin_dir(name) {
            roots.push((dir, None));
        }
    }
    roots
});

/// Refuses anything that isn't strictly inside `home` or one of the allowed outside folders, a
/// protected top-level folder, a symlink, or (outside home) something the user doesn't own.
pub fn guard(home: &Path, path: &Path) -> Result<(), String> {
    let refuse = || format!("refusing to remove {}", path.display());
    // How many levels below `root` the path is, if it's under it at all.
    let depth_under = |root: &Path| -> Option<Result<usize, String>> {
        let rel = path.strip_prefix(root).ok()?;
        let normal = !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)));
        Some(if normal { Ok(rel.components().count()) } else { Err(refuse()) })
    };
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(depth) = depth_under(home) {
        let rel = path.strip_prefix(home).unwrap_or(path);
        if depth? == 1 && PROTECTED.iter().any(|p| rel == Path::new(p)) {
            return Err(format!("refusing to remove ~/{}", rel.display()));
        }
    } else {
        let (depth, max) = OUTSIDE
            .iter()
            .find_map(|(root, max)| depth_under(root).map(|d| (d, *max)))
            .ok_or_else(|| format!("{} is outside the folders Storage cleans", path.display()))?;
        let depth = depth?;
        if max.is_some_and(|m| depth > m) {
            return Err(refuse());
        }
        if meta.uid() != sys::uid() {
            return Err(format!("{} belongs to another user", path.display()));
        }
    }
    if meta.file_type().is_symlink() {
        return Err(format!("{} is now a symlink; skipped", path.display()));
    }
    Ok(())
}

/// `~/path` for home-relative paths, the path itself otherwise.
fn shown(path: &str) -> String {
    if path.starts_with('/') { path.to_owned() } else { format!("~/{path}") }
}

/// Clears one entry. Returns how it was cleared.
pub fn clean(home: &Path, e: &Entry) -> Result<Method, String> {
    let how = method(e);
    match how {
        Method::Command => {
            let argv = e.cmd.as_deref().unwrap_or_default();
            let (prog, args) = argv.split_first().ok_or("empty command")?;
            let mut cmd = Command::new(prog);
            cmd.args(args);
            let out = sys::run(cmd, None).ok_or_else(|| format!("couldn't run {prog}"))?;
            if !out.ok {
                let msg = out.stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_owned();
                return Err(format!("{}: {msg}", argv.join(" ")));
            }
        }
        Method::Empty => {
            let trash = home.join(".Trash");
            let mut first_err = None;
            for item in fs::read_dir(&trash).map_err(|e| format!("~/.Trash: {e}"))?.flatten() {
                if let Err(err) = force_remove(&item.path()) {
                    first_err.get_or_insert_with(|| format!("{}: {err}", item.path().display()));
                }
            }
            if let Some(err) = first_err {
                return Err(err);
            }
        }
        Method::Delete => {
            if !exists(&e.abs) {
                return Ok(how);
            }
            guard(home, &e.abs)?;
            force_remove(&e.abs).map_err(|err| format!("{}: {err}", shown(&e.path)))?;
        }
        Method::Trash => {
            if !exists(&e.abs) {
                return Ok(how);
            }
            guard(home, &e.abs)?;
            move_to_trash(&e.abs).map_err(|err| format!("{}: {err}", shown(&e.path)))?;
            if let Some(gitdir) = &e.gitdir {
                git::prune(gitdir);
            }
        }
    }
    Ok(how)
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn move_to_trash(p: &Path) -> Result<(), String> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    // NSFileManager is fast and doesn't need permission to control Finder.
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(p).map_err(|e| e.to_string())
}

/// `rm -rf`, including read-only folders (Go's module cache is read-only by design).
/// Never follows symlinks.
pub fn force_remove(p: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if !meta.is_dir() {
        return fs::remove_file(p);
    }
    fs::remove_dir_all(p).or_else(|_| {
        make_writable(p);
        fs::remove_dir_all(p)
    })
}

fn make_writable(dir: &Path) {
    let Ok(meta) = fs::symlink_metadata(dir) else { return };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = fs::set_permissions(dir, Permissions::from_mode(mode | 0o700));
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                make_writable(&e.path());
            }
        }
    }
}
