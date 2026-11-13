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

use crate::bulk::{self, Kind};
use crate::git;
use crate::remove;
use crate::rules::{self, Tier};
use crate::scan::Entry;
use crate::sys;
use serde::Serialize;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
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
/// protected top-level folder, a symlink, something reached through a symlink below that root, or
/// (outside home) something the user doesn't own. Returns the root the path is under.
pub fn guard(home: &Path, path: &Path) -> Result<PathBuf, String> {
    let refuse = || format!("refusing to remove {}", path.display());
    // How many levels below `root` the path is, if it's under it at all.
    let depth_under = |root: &Path| -> Option<Result<usize, String>> {
        let rel = path.strip_prefix(root).ok()?;
        let normal = !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)));
        Some(if normal { Ok(rel.components().count()) } else { Err(refuse()) })
    };
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let root = if let Some(depth) = depth_under(home) {
        let rel = path.strip_prefix(home).unwrap_or(path);
        if depth? == 1 && PROTECTED.iter().any(|p| rel == Path::new(p)) {
            return Err(format!("refusing to remove ~/{}", rel.display()));
        }
        home.to_path_buf()
    } else {
        let (root, depth, max) = OUTSIDE
            .iter()
            .find_map(|(root, max)| depth_under(root).map(|d| (root, d, *max)))
            .ok_or_else(|| format!("{} is outside the folders Storage cleans", path.display()))?;
        let depth = depth?;
        if max.is_some_and(|m| depth > m) {
            return Err(refuse());
        }
        if meta.uid() != sys::uid() {
            return Err(format!("{} belongs to another user", path.display()));
        }
        root.clone()
    };
    if meta.file_type().is_symlink() {
        return Err(format!("{} is now a symlink; skipped", path.display()));
    }
    // The path only looks contained if no folder on the way down is a link to somewhere else.
    let parent = path.parent().unwrap_or(path);
    let lexical = fs::canonicalize(&root).map(|r| r.join(parent.strip_prefix(&root).unwrap_or(Path::new(""))));
    if fs::canonicalize(parent).ok() != lexical.ok() {
        return Err(format!("{} is reached through a symlink; skipped", path.display()));
    }
    Ok(root)
}

/// `~/path` for home-relative paths, the path itself otherwise.
fn shown(path: &str) -> String {
    if path.starts_with('/') { path.to_owned() } else { format!("~/{path}") }
}

/// Why a folder's newest modification time can't be known.
#[derive(Debug, PartialEq, Eq)]
enum Unseen {
    /// It, or a folder in it, can't be listed, or a listing came back with gaps.
    Unreadable,
    /// It holds more entries than the check is allowed to look at.
    TooMany,
}

/// How many entries the "still untouched?" check looks at before giving up.
const MTIME_BUDGET: u32 = 200_000;

/// Newest modification time of `path` and, for a folder, of anything in it. An error when that
/// can't be known: such a folder could hold anything, written at any time.
fn newest_mtime(path: &Path, budget: &mut u32) -> Result<i64, Unseen> {
    let md = fs::symlink_metadata(path).map_err(|_| Unseen::Unreadable)?;
    let mut newest = md.mtime();
    // A file is its own answer. Only a folder has contents to look through.
    if !md.is_dir() {
        return Ok(newest);
    }
    let Ok((items, 0)) = bulk::list_counting(path, false) else { return Err(Unseen::Unreadable) };
    for item in items {
        if *budget == 0 {
            return Err(Unseen::TooMany);
        }
        *budget -= 1;
        newest = newest.max(match item.kind {
            Kind::Dir => newest_mtime(&path.join(&item.name), budget)?,
            _ => item.mtime,
        });
    }
    Ok(newest)
}

/// The scan's reasons for offering `e` are checked again, because the report can be hours old:
/// the same thing is still at the path, nothing has been written to a folder offered for being
/// stale, and an old CLI version hasn't become the current one.
fn recheck(home: &Path, e: &Entry) -> Result<(), String> {
    recheck_with(home, e, &sys::running_apps)
}

/// `recheck`, with the list of running apps coming from `running`. It is asked afresh for every
/// cache entry and never remembered: an app started while a cleanup is under way has to be seen
/// before its own cache is reached.
fn recheck_with(home: &Path, e: &Entry, running: &dyn Fn() -> std::collections::HashSet<String>) -> Result<(), String> {
    let changed = |what: &str| Err(format!("{}: {what} since the scan; scan again", shown(&e.path)));
    if let (Some(seen), Ok(now)) = (e.ident, fs::symlink_metadata(&e.abs))
        && seen != (now.dev(), now.ino())
    {
        return changed("was replaced");
    }
    if e.min_age > 0 {
        // Neither refusal says "scan again": another scan would offer it again and this would
        // refuse it again.
        let unseen = |why: &str| Err(format!("{}: {why}; remove it yourself", shown(&e.path)));
        let mut budget = MTIME_BUDGET;
        match newest_mtime(&e.abs, &mut budget) {
            Ok(newest) if newest > e.scanned => return changed("was written to"),
            Ok(_) => {}
            Err(Unseen::Unreadable) => {
                return unseen("part of it can't be read, so Storage can't tell whether it is still untouched");
            }
            Err(Unseen::TooMany) => {
                return unseen("it holds too many files to check that it is still untouched");
            }
        }
    }
    if e.rec == "oldversions" {
        let target = fs::canonicalize(&e.abs).ok();
        let links =
            [home.join(".local/bin/claude"), e.abs.parent().and_then(Path::parent).unwrap_or(&e.abs).join("current")];
        let current = links.iter().filter_map(|l| fs::canonicalize(l).ok());
        if current.into_iter().any(|c| target.as_deref().is_some_and(|t| c.starts_with(t))) {
            return changed("became the version in use");
        }
    }
    // Things that were free to go when scanned and aren't any more: a version that has become the
    // default, a cache whose app has been started.
    if e.hold.is_none() {
        if e.rec == "toolchains" && crate::scan::default_toolchains(home).contains(&e.path) {
            return changed("became the default version");
        }
        let app = e.label.as_deref().and_then(|l| l.split(": ").next());
        if let Some(app) = app
            && matches!(e.rec, "appcaches" | "electron" | "containercaches" | "syscaches")
            && crate::scan::app_is_running(&running(), app)
        {
            return changed("its app was started");
        }
    }
    Ok(())
}

/// A worktree is looked at again just before it goes: it may have gained work, or company, since
/// the scan. Anything the user wasn't shown stops it: a process now sitting in it, a different
/// commit, a different set of changed files, a lock, git not answering. A detached HEAD with
/// commits of its own gets a ref first. Returns what git said, for the caller to act on.
fn recheck_worktree(home: &Path, e: &Entry, gitdir: &Path) -> Result<Option<git::Worktree>, String> {
    let Some(seen) = &e.worktree else { return Ok(None) };
    let stop = |why: &str| Err(format!("{}: {why}; scan again", shown(&e.path)));
    if !seen.busy {
        match sys::process_cwds() {
            None => return stop("couldn't check whether anything is running in it"),
            Some(cwds) if cwds.iter().any(|cwd| cwd.starts_with(&e.abs)) => {
                return stop("something is running in it now");
            }
            Some(_) => {}
        }
    }
    // Whether git has a record of it has to be what the user was shown, too: a repository that
    // came back can now say what is uncommitted, and one that went can no longer vouch for anything.
    if seen.orphaned != !gitdir.is_dir() {
        return stop("its repository appeared or disappeared since the scan");
    }
    if seen.orphaned {
        return Ok(None); // git has no record of it, so there's nothing to ask or save
    }
    let now = git::inspect(&e.abs, &e.path, gitdir, home);
    // Cleanup acts on what the user was shown. A look that couldn't see everything, at the scan
    // or now, proves nothing about what changed in between, whatever the other look says.
    if seen.partial || now.partial {
        return Err(format!(
            "{}: part of it is unreadable or too large to check for changes, so remove it yourself",
            shown(&e.path)
        ));
    }
    if now.unknown && !seen.unknown {
        return stop("git couldn't check it");
    }
    // The scan couldn't ask git and showed it as "not checked"; now git answers, with details
    // nobody has seen.
    if seen.unknown && !now.unknown {
        return stop("git can inspect it now, and what it holds hasn't been shown");
    }
    if now.locked && !seen.locked {
        return stop("it has been locked");
    }
    if !seen.unknown && (now.head != seen.head || now.fingerprint != seen.fingerprint || now.branch != seen.branch) {
        return stop("its commits or files changed since the scan");
    }
    git::rescue(gitdir).map_err(|err| format!("{}: {err}; left in place", shown(&e.path)))?;
    Ok(Some(now))
}

/// A simulator is deleted by `simctl`, which doesn't ask. It only goes if it can be shown to be
/// the idle device that was offered: shut down, and not used since the scan. A device whose record
/// can't be read is left alone.
fn recheck_simulator(e: &Entry) -> Result<(), String> {
    let name = e.label.as_deref().unwrap_or("The simulator");
    let device: plist::Dictionary = plist::from_file(e.abs.join("device.plist"))
        .map_err(|_| format!("{name}: couldn't read its state; left alone"))?;
    // 1 is shut down; 3 is booted, and the others are on the way up or down.
    match device.get("state").and_then(|v| v.as_unsigned_integer()) {
        Some(1) => {}
        Some(3) => return Err(format!("{name} is booted; shut it down first")),
        _ => return Err(format!("{name} isn't shut down; left alone")),
    }
    if simulator_last_used(&device).is_some_and(|t| t > e.scanned) {
        return Err(format!("{name} was used since the scan; scan again"));
    }
    Ok(())
}

/// When a simulator was last used, from its `device.plist`. CoreSimulator writes `lastUsedAt`;
/// older versions wrote `lastBootedAt`.
pub fn simulator_last_used(device: &plist::Dictionary) -> Option<i64> {
    ["lastUsedAt", "lastBootedAt"]
        .iter()
        .filter_map(|key| device.get(key).and_then(|v| v.as_date()))
        .filter_map(|d| std::time::SystemTime::from(d).duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .max()
}

/// Renames `from` to `to` only if nothing is at `to`: never replaces a file.
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    };
    let (from, to) = (c(from)?, c(to)?);
    // SAFETY: both are valid NUL-terminated paths.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Moves everything in `dir` except `spare` to the Trash as one folder (so it can be put back as
/// one), leaving `dir` behind for the spared folder. If the Trash doesn't take it, what was
/// gathered is put back where it was, without replacing anything written there in the meantime:
/// an agent can start a session in that folder at any moment.
fn trash_all_but(dir: &Path, spare: &str, move_to_trash: &dyn Fn(&Path) -> Result<(), String>) -> Result<(), String> {
    let items = bulk::list(dir, false).map_err(|e| e.to_string())?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let holding = dir.with_file_name(format!("{name} (cleared {})", sys::now()));
    fs::create_dir(&holding).map_err(|e| e.to_string())?;
    let mut moved: Vec<&str> = Vec::new();
    let mut outcome = Ok(());
    for item in items.iter().filter(|i| i.name != spare) {
        match rename_new(&dir.join(&item.name), &holding.join(&item.name)) {
            Ok(()) => moved.push(&item.name),
            Err(e) => {
                outcome = Err(e.to_string());
                break;
            }
        }
    }
    if outcome.is_ok() && !moved.is_empty() {
        outcome = move_to_trash(&holding);
        if outcome.is_ok() && !holding.exists() {
            return Ok(());
        }
        // "Done", and yet the folder is still here: nothing went anywhere.
        outcome = outcome.and(Err("the Trash didn't take it".into()));
    }
    // Undo the gathering. Anything whose place has been taken stays in the holding folder.
    let stuck = moved.iter().filter(|n| rename_new(&holding.join(n), &dir.join(n)).is_err()).count();
    if stuck == 0 {
        let _ = fs::remove_dir(&holding);
        return outcome;
    }
    let why = outcome.err().unwrap_or_else(|| "couldn't finish".into());
    Err(format!(
        "{why}; {stuck} of its items couldn't go back because something new has their names, and are in {}",
        holding.display()
    ))
}

/// Puts a batch in the order it must run: the Trash is emptied before anything is moved into it,
/// because what a cleanup sends there was promised to be recoverable.
pub fn plan(entries: &mut [Entry]) {
    entries.sort_by_key(|e| e.rec != "trash");
}

/// Clears one entry. Returns how it was cleared.
pub fn clean(home: &Path, e: &Entry) -> Result<Method, String> {
    clean_with(home, e, &move_to_trash)
}

/// [`clean`] with the move to the Trash supplied by the caller, so tests can use a Trash of their own.
pub fn clean_with(
    home: &Path,
    e: &Entry,
    move_to_trash: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Method, String> {
    let how = method(e);
    let fail = |err: io::Error| format!("{}: {err}", shown(&e.path));
    match how {
        Method::Command => {
            let argv = e.cmd.as_deref().unwrap_or_default();
            let (prog, args) = argv.split_first().ok_or("empty command")?;
            if e.rec == "simulators" {
                recheck_simulator(e)?;
            }
            let mut cmd = Command::new(prog);
            // Whatever the shell that launched the app pointed Docker at, this is about this Mac.
            cmd.args(args).env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
            let out = sys::run(cmd, Some(rules::command_timeout(e.rec)))
                .ok_or_else(|| format!("{}: couldn't run, or took too long and was stopped", argv.join(" ")))?;
            if !out.ok {
                let msg = out.stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_owned();
                return Err(format!("{}: {msg}", argv.join(" ")));
            }
        }
        // Refuses a Trash that is a link to somewhere else, and never follows one inside it.
        Method::Empty => remove::empty_under(home, Path::new(".Trash")).map_err(|err| format!("~/.Trash: {err}"))?,
        Method::Delete => {
            if !exists(&e.abs) {
                return Ok(how);
            }
            let root = guard(home, &e.abs)?;
            recheck(home, e)?;
            remove::remove_under(&root, &e.abs, e.ident).map_err(fail)?;
        }
        Method::Trash => {
            if !exists(&e.abs) {
                return Ok(how);
            }
            let root = guard(home, &e.abs)?;
            recheck(home, e)?;
            let fresh = match &e.gitdir {
                Some(gitdir) => recheck_worktree(home, e, gitdir)?,
                None => None,
            };
            // Uncommitted work: what is staged is saved as a commit first, because the index goes
            // with git's record of the worktree whenever git next tidies up. If that can't be done
            // (unresolved merge, a stale index.lock), the record is locked instead, which git never
            // prunes. A lock that was already there is left as it is, and one written here is
            // taken off again if the move doesn't happen.
            let mut lock = None;
            if let (Some(gitdir), Some(now)) = (&e.gitdir, &fresh)
                && now.dirty > 0
                && git::rescue_index(gitdir).is_err()
                && !gitdir.join("locked").exists()
            {
                let note = "Moved to the Trash by Storage with staged changes that couldn't be saved as a commit.";
                fs::write(gitdir.join("locked"), note)
                    .map_err(|_| format!("{}: couldn't protect its staged changes; left in place", shown(&e.path)))?;
                lock = Some(gitdir.join("locked"));
            }
            let moved = match &e.spare {
                Some(spare) => trash_all_but(&e.abs, spare, move_to_trash),
                None => {
                    // The Trash takes a path, not a descriptor, so the checked object is held open
                    // across the move and asked afterwards where it went. Still where it was means
                    // something else sat at that path for the instant that mattered.
                    let held = remove::hold(&root, &e.abs, e.ident).map_err(|err| err.to_string());
                    held.and_then(|held| {
                        let before = held.as_ref().and_then(remove::Held::path);
                        move_to_trash(&e.abs)?;
                        match (&held, &before) {
                            (Some(held), Some(before)) if held.path().as_ref() == Some(before) => Err(
                                "something else was at this path and was moved to the Trash instead; put it back from there"
                                    .into(),
                            ),
                            _ => Ok(()),
                        }
                    })
                }
            };
            if let (Err(_), Some(lock)) = (&moved, &lock) {
                let _ = fs::remove_file(lock);
            }
            moved.map_err(|err| format!("{}: {err}", shown(&e.path)))?;
            // git's record goes only when the worktree held nothing uncommitted. With changes, the
            // record is what lets a folder put back from the Trash work again straight away.
            if let (Some(gitdir), Some(now)) = (&e.gitdir, &fresh)
                && now.dirty == 0
                && !now.unknown
            {
                git::forget(gitdir, &e.abs);
            }
            // An Android emulator is a folder plus a sibling `.ini` that points at it.
            if e.rec == "android" && e.abs.extension().is_some_and(|x| x == "avd") {
                let ini = e.abs.with_extension("ini");
                if exists(&ini) && guard(home, &ini).is_ok() {
                    let _ = move_to_trash(&ini);
                }
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
    remove::remove(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashSet;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("storage-clean-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_app_started_between_two_caches_is_seen() {
        let home = scratch("apps");
        let cache = |app: &str| Entry {
            rec: "appcaches",
            path: format!("Library/Caches/{app}"),
            label: Some(format!("{app}: cache")),
            abs: home.join("Library/Caches").join(app),
            ..Entry::default()
        };
        // Nothing is running when the first cache is checked. The second app starts before its
        // own cache is reached, a moment later in the same cleanup.
        let asked = Cell::new(0);
        let running = || {
            asked.set(asked.get() + 1);
            if asked.get() == 1 { HashSet::new() } else { HashSet::from(["second".to_owned()]) }
        };
        assert_eq!(recheck_with(&home, &cache("first"), &running), Ok(()));
        let err = recheck_with(&home, &cache("second"), &running).unwrap_err();
        assert!(err.contains("its app was started"), "{err}");
        assert_eq!(asked.get(), 2, "asked once per cache entry, never reused");
        // An entry the scan already held for a running app was confirmed with that warning.
        let held = Entry { hold: Some("app is running".into()), ..cache("second") };
        assert_eq!(recheck_with(&home, &held, &running), Ok(()));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_folder_too_large_to_check_says_so() {
        let home = scratch("budget");
        for i in 0..5 {
            fs::write(home.join(format!("f{i}")), b"x").unwrap();
        }
        assert_eq!(newest_mtime(&home, &mut 4), Err(Unseen::TooMany));
        assert!(newest_mtime(&home, &mut 5).is_ok());
        assert!(newest_mtime(&home.join("f0"), &mut 0).is_ok(), "a file is its own answer");
        assert_eq!(newest_mtime(&home.join("gone"), &mut 5), Err(Unseen::Unreadable));
        fs::remove_dir_all(&home).unwrap();
    }
}
