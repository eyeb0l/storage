//! Linked git worktrees: detection and a read-only look at what removing one would lose.

use crate::rules;
use crate::sys;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Entries the change digest will look through in a worktree's untracked and ignored folders.
const STAMP_BUDGET: u32 = 200_000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Worktree {
    /// The coding agent that made it, from its location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<&'static str>,
    /// The main repository, `~`-relative when it's in the home folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Checked-out branch; `None` for a detached HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Files with uncommitted changes, including untracked files that aren't ignored.
    pub dirty: u32,
    /// Commits on a detached HEAD that no branch or remote contains; lost with the worktree.
    pub unique: u32,
    /// HEAD is already part of the default branch.
    pub merged: bool,
    /// The branch has an upstream and no commits that aren't on it.
    pub pushed: bool,
    /// The branch's upstream was deleted on the remote, usually because its PR was merged
    /// (squash merges never show up as `merged`).
    pub gone: bool,
    /// The repo no longer knows about this worktree (deleted or pruned), so git can't inspect it.
    /// Its contents were not checked.
    pub orphaned: bool,
    /// git couldn't be asked (missing, timed out, refused), so nothing above is known. Never
    /// treated as clean.
    pub unknown: bool,
    /// Locked with `git worktree lock`: someone asked for it to be kept.
    pub locked: bool,
    /// A running process has its working directory inside it.
    pub busy: bool,
    /// Ignored files that aren't build output or dependencies (`.env`, local config). They exist
    /// only here and git won't bring them back.
    pub ignored: u32,
    /// A few of their names, for the UI.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignored_names: Vec<String>,
    /// The commit HEAD was on, and a digest of what `git status` listed. Cleanup compares both
    /// with a fresh look: the same number of changed files can still be different work.
    #[serde(skip)]
    pub head: String,
    #[serde(skip)]
    pub fingerprint: u64,
    /// The digest couldn't cover everything (an ignored folder too large or unreadable), so it
    /// can't show that nothing changed. Also reported as `unknown`; cleanup always refuses, so
    /// the UI says so and doesn't offer it.
    pub partial: bool,
    /// Last git activity (index, HEAD or reflog), unix seconds.
    pub last_active: i64,
}

/// If `dotgit` is a `.git` *file* pointing into another repo's `worktrees/` folder, returns that git dir.
/// Submodules also use `.git` files, but point into `modules/`, so they're ignored.
pub fn linked_gitdir(dotgit: &Path) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(dotgit).ok()?;
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    let text = fs::read_to_string(dotgit).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let path = Path::new(target);
    let path = if path.is_absolute() { path.to_path_buf() } else { dotgit.parent()?.join(path) };
    path.parent().and_then(Path::file_name).is_some_and(|n| n == "worktrees").then_some(path)
}

/// The repository's shared git dir (`repo/.git`) for a worktree's git dir (`repo/.git/worktrees/<name>`).
pub fn common_dir(gitdir: &Path) -> Option<PathBuf> {
    if let Ok(rel) = fs::read_to_string(gitdir.join("commondir")) {
        let p = gitdir.join(rel.trim());
        return Some(fs::canonicalize(&p).unwrap_or(p));
    }
    Some(gitdir.parent()?.parent()?.to_path_buf())
}

pub fn agent_for(rel: &str) -> Option<&'static str> {
    const AGENTS: &[(&str, &str)] = &[
        (".t3/worktrees/", "T3 Code"),
        (".claude/worktrees/", "Claude Code"),
        (".codex/worktrees/", "Codex"),
        (".cursor/worktrees/", "Cursor"),
        ("conductor/workspaces/", "Conductor"),
        (".gemini/", "Gemini"),
    ];
    let rel = format!("/{rel}");
    AGENTS.iter().find(|(pat, _)| rel.contains(&format!("/{pat}"))).map(|&(_, name)| name)
}

fn git(dir: &Path, args: &[&str]) -> Option<(bool, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C");
    sys::run(cmd, Some(GIT_TIMEOUT)).map(|o| (o.ok, o.stdout))
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).and_then(|(ok, out)| ok.then(|| out.trim().to_owned()))
}

/// Inspects a worktree without changing anything (`GIT_OPTIONAL_LOCKS=0` keeps `git status` from writing the index).
pub fn inspect(wt: &Path, rel: &str, gitdir: &Path, home: &Path) -> Worktree {
    let mut info = Worktree { agent: agent_for(rel), ..Default::default() };
    info.last_active = ["index", "HEAD", "logs/HEAD"]
        .iter()
        .map(|f| sys::mtime(&gitdir.join(f)))
        .chain([sys::mtime(wt)])
        .max()
        .unwrap_or(0);
    if let Some(common) = common_dir(gitdir) {
        let repo =
            if common.file_name().is_some_and(|n| n == ".git") { common.parent().unwrap_or(&common) } else { &common };
        info.repo = Some(match repo.strip_prefix(home) {
            Ok(r) => format!("~/{}", r.display()),
            Err(_) => repo.display().to_string(),
        });
    }
    if !gitdir.is_dir() {
        info.orphaned = true;
        return info;
    }
    info.locked = gitdir.join("locked").exists();

    // From here on a failed command means "don't know", which must never read as "nothing to lose".
    let Some(head) = git_out(wt, &["rev-parse", "--abbrev-ref", "HEAD"]) else {
        info.unknown = true;
        return info;
    };
    info.branch = (head != "HEAD").then_some(head);
    info.head = git_out(wt, &["rev-parse", "HEAD"]).unwrap_or_default();
    // Version 2 of the format carries what is staged (index object ids), `-z` gives file names as
    // they are (no quoting of spaces or accents), and every untracked file is listed on its own.
    // Ignored things are listed once where the ignore rule matched, so `node_modules` is one line.
    match git(wt, &["status", "--porcelain=v2", "-z", "--untracked-files=all", "--ignored=matching"]) {
        Some((true, status)) => {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            status.hash(&mut hasher);
            let mut budget = STAMP_BUDGET;
            let mut records = status.split('\0');
            while let Some(record) = records.next() {
                let Some((kind, rest)) = record.split_once(' ') else { continue };
                let path = match kind {
                    "1" => rest.splitn(8, ' ').nth(7),
                    // A rename is followed by a second record holding the old name.
                    "2" => {
                        records.next();
                        rest.splitn(9, ' ').nth(8)
                    }
                    "u" => rest.splitn(10, ' ').nth(9),
                    "?" | "!" => Some(rest),
                    _ => None,
                };
                let Some(path) = path else { continue };
                if kind == "!" {
                    let path = path.trim_end_matches('/');
                    let name = path.rsplit('/').next().unwrap_or(path);
                    // Tauri writes its schemas to `src-tauri/gen` on every build.
                    if rules::is_regenerable(name) || format!("/{path}/").contains("/src-tauri/gen/") {
                        continue;
                    }
                    info.ignored += 1;
                    if info.ignored_names.len() < 3 {
                        info.ignored_names.push(path.to_owned());
                    }
                } else {
                    info.dirty += 1;
                }
                // The list doesn't change when a listed file is edited again, so what each one
                // weighs and when it last changed goes into the digest too (a folder: everything in it).
                if !stamp(&wt.join(path), &mut hasher, &mut budget) {
                    // Too much to look through, or part of it unreadable: say so, never "unchanged".
                    info.partial = true;
                    info.unknown = true;
                }
            }
            info.fingerprint = hasher.finish();
        }
        _ => info.unknown = true,
    }
    if info.branch.is_none() {
        match git_out(wt, &["rev-list", "--count", "HEAD", "--not", "--branches", "--remotes", "--tags"])
            .and_then(|n| n.parse().ok())
        {
            Some(n) => info.unique = n,
            None => info.unknown = true,
        }
    }
    if let Some(branch) = &info.branch {
        let track =
            git_out(wt, &["for-each-ref", "--format=%(upstream)|%(upstream:track)", &format!("refs/heads/{branch}")]);
        if let Some((upstream, track)) = track.as_deref().and_then(|t| t.split_once('|')) {
            info.gone = !upstream.is_empty() && track.contains("gone");
            info.pushed = !upstream.is_empty() && !info.gone && !track.contains("ahead");
        }
    }
    let default = git_out(wt, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .filter(|s| !s.is_empty() && s != "origin/HEAD")
        .or_else(|| {
            ["main", "master", "trunk"]
                .into_iter()
                .find(|b| {
                    git(wt, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_some_and(|(ok, _)| ok)
                })
                .map(str::to_owned)
        });
    if let Some(default) = default {
        info.merged = git(wt, &["merge-base", "--is-ancestor", "HEAD", &default]).is_some_and(|(ok, _)| ok);
    }
    info
}

/// Before a worktree with a detached HEAD goes to the Trash: if it has commits no branch, remote
/// or tag contains, points a ref at its HEAD so they stay reachable (git would otherwise collect
/// them once its record of the worktree is gone, and putting the folder back wouldn't bring them
/// back). Returns the ref's name, or `None` when there is nothing only this worktree holds.
/// The name carries the commit, so two worktrees can never overwrite each other's ref.
pub fn rescue(gitdir: &Path) -> Result<Option<String>, String> {
    let run = |dir: &Path, args: &[&str]| -> Option<(bool, String)> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir").arg(dir).args(args).env("LC_ALL", "C");
        sys::run(cmd, Some(GIT_TIMEOUT)).map(|o| (o.ok, o.stdout.trim().to_owned()))
    };
    let on_branch = run(gitdir, &["symbolic-ref", "-q", "HEAD"]).ok_or("git didn't answer")?.0;
    if on_branch {
        return Ok(None);
    }
    let only_here = run(gitdir, &["rev-list", "--count", "HEAD", "--not", "--branches", "--remotes", "--tags"])
        .and_then(|(ok, out)| ok.then(|| out.parse::<u32>().ok()).flatten())
        .ok_or("couldn't tell whether its commits are saved elsewhere")?;
    if only_here == 0 {
        return Ok(None);
    }
    let sha = run(gitdir, &["rev-parse", "--verify", "HEAD"])
        .and_then(|(ok, out)| ok.then_some(out))
        .filter(|s| s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("couldn't read its HEAD")?;
    let common = common_dir(gitdir).filter(|c| c.is_dir()).ok_or("its repository is missing")?;
    let name = gitdir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    let refname = format!("refs/storage/trashed/{name}-{}", &sha[..12]);
    match run(&common, &["update-ref", &refname, &sha]) {
        Some((true, _)) => Ok(Some(refname)),
        _ => Err("couldn't save a ref to its commits".into()),
    }
}

/// Before a worktree with uncommitted work goes to the Trash: if anything is staged, saves the
/// index as a commit under `refs/storage/trashed/…`. A file that was staged and then edited again
/// exists in that version only in the index, and git drops its record of a worktree whose folder
/// has gone (index and all) whenever it next tidies up, so keeping the record isn't enough.
/// Returns the ref's name, or `None` when nothing is staged.
pub fn rescue_index(gitdir: &Path) -> Result<Option<String>, String> {
    let run = |dir: &Path, args: &[&str]| -> Option<(bool, String)> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir").arg(dir).args(args).env("LC_ALL", "C");
        for who in ["AUTHOR", "COMMITTER"] {
            cmd.env(format!("GIT_{who}_NAME"), "Storage").env(format!("GIT_{who}_EMAIL"), "storage@localhost");
        }
        sys::run(cmd, Some(GIT_TIMEOUT)).map(|o| (o.ok, o.stdout.trim().to_owned()))
    };
    let ok = |out: Option<(bool, String)>, what: &str| match out {
        Some((true, text)) => Ok(text),
        _ => Err(format!("couldn't {what}")),
    };
    if run(gitdir, &["diff", "--cached", "--quiet"]).ok_or("git didn't answer")?.0 {
        return Ok(None);
    }
    // Fails while a merge has unresolved files; the caller then keeps the record from being pruned.
    let tree = ok(run(gitdir, &["write-tree"]), "save what is staged")?;
    let name = gitdir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let message = format!("Staged changes of the worktree {name} when Storage moved it to the Trash");
    let commit = ok(run(gitdir, &["commit-tree", &tree, "-p", "HEAD", "-m", &message]), "save what is staged")?;
    if commit.len() < 12 {
        return Err("couldn't save what is staged".into());
    }
    let common = common_dir(gitdir).filter(|c| c.is_dir()).ok_or("its repository is missing")?;
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    let refname = format!("refs/storage/trashed/{name}-staged-{}", &commit[..12]);
    ok(run(&common, &["update-ref", &refname, &commit]), "save a ref to what is staged")?;
    Ok(Some(refname))
}

/// Adds a path to the digest that tells whether a worktree's uncommitted work has changed: its
/// length and modification time to the nanosecond, and for a folder the same for everything in it
/// by name. Returns false when it couldn't see all of it (a folder that can't be listed, or more
/// than `budget` entries), in which case the digest proves nothing.
fn stamp(path: &Path, hasher: &mut impl std::hash::Hasher, budget: &mut u32) -> bool {
    use std::hash::Hash;
    use std::os::unix::fs::MetadataExt;
    let md = match fs::symlink_metadata(path) {
        Ok(md) => md,
        // Listed a moment ago and gone now: that is a change, and the next look will differ too.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            0u8.hash(hasher);
            return true;
        }
        // There, but it won't say what it is (no permission to read its attributes, a path too
        // long to reach): it could change without this ever noticing.
        Err(_) => return false,
    };
    (md.len(), md.mtime(), md.mtime_nsec(), md.is_dir()).hash(hasher);
    if !md.is_dir() {
        return true;
    }
    // A listing that left entries out is as blind as one that failed.
    let Ok((mut items, 0)) = crate::bulk::list_counting(path, false) else { return false };
    items.sort_by(|a, b| a.name.cmp(&b.name));
    for item in items {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        item.name.hash(hasher);
        if !stamp(&path.join(&item.name), hasher, budget) {
            return false;
        }
    }
    true
}

/// Drops git's record of one worktree whose folder has gone, so its branch can be checked out
/// elsewhere. Only that worktree's record: other stale ones (say, on an unplugged disk) are left
/// for git to expire. Does nothing unless the record really points at `wt`.
pub fn forget(gitdir: &Path, wt: &Path) {
    let is_record = gitdir.parent().and_then(Path::file_name).is_some_and(|n| n == "worktrees");
    let points_here = fs::read_to_string(gitdir.join("gitdir")).is_ok_and(|p| Path::new(p.trim()) == wt.join(".git"));
    if is_record && points_here && !wt.exists() {
        let _ = crate::remove::remove(gitdir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_from_paths() {
        assert_eq!(agent_for(".t3/worktrees/storage/t3-1"), Some("T3 Code"));
        assert_eq!(agent_for("Developer/app/.claude/worktrees/fix"), Some("Claude Code"));
        assert_eq!(agent_for(".codex/worktrees/2016/storage"), Some("Codex"));
        assert_eq!(agent_for("conductor/workspaces/app/oslo"), Some("Conductor"));
        assert_eq!(agent_for("Developer/app-wt"), None);
    }
}
