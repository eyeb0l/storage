//! Linked git worktrees: detection and a read-only look at what removing one would lose.

use crate::sys;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(20);

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
    pub orphaned: bool,
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

    let Some(head) = git_out(wt, &["rev-parse", "--abbrev-ref", "HEAD"]) else {
        info.orphaned = true;
        return info;
    };
    info.branch = (head != "HEAD").then_some(head);
    if let Some((true, status)) = git(wt, &["status", "--porcelain", "--untracked-files=normal"]) {
        info.dirty = status.lines().filter(|l| !l.trim().is_empty()).count() as u32;
    }
    if info.branch.is_none() {
        info.unique = git_out(wt, &["rev-list", "--count", "HEAD", "--not", "--branches", "--remotes", "--tags"])
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
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

/// Drops git's record of worktrees whose folders are gone. Safe: it never touches files.
pub fn prune(gitdir: &Path) {
    if let Some(common) = common_dir(gitdir).filter(|c| c.is_dir()) {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir").arg(common).args(["worktree", "prune"]);
        let _ = sys::run(cmd, Some(GIT_TIMEOUT));
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
