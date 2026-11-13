//! End-to-end checks against a fake home folder built in a temp directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use storage_core::{Cat, Report, Scanner, clean};

struct TempHome(PathBuf);

impl TempHome {
    fn new(tag: &str) -> Self {
        // Tests run in parallel and macOS clocks only resolve microseconds, so add a counter.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("storage-test-{tag}-{}-{n}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        TempHome(fs::canonicalize(dir).unwrap())
    }
    /// Writes a file big enough to clear the 1 MB entry threshold.
    fn big(&self, rel: &str) {
        self.file(rel, &vec![0xAB; 1_200_000]);
    }
    fn file(&self, rel: &str, data: &[u8]) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
    fn git(&self, dir: &str, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(self.0.join(dir))
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }
}

impl TempHome {
    /// Backdates a file, as if it had been sitting there for `days`.
    fn age(&self, rel: &str, days: u64) {
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        fs::File::options().write(true).open(self.0.join(rel)).unwrap().set_modified(then).unwrap();
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = clean::force_remove(&self.0);
    }
}

fn scan(home: &Path) -> Report {
    Scanner::new(home).unwrap().home_only().run()
}

fn entry<'a>(r: &'a Report, rec: &str, path: &str) -> &'a storage_core::Entry {
    r.entries.iter().find(|e| e.rec == rec && e.path == path).unwrap_or_else(|| {
        panic!("no {rec} entry for {path}; got {:#?}", r.entries.iter().map(|e| (&e.rec, &e.path)).collect::<Vec<_>>())
    })
}

fn fixture() -> TempHome {
    let h = TempHome::new("scan");
    h.file("Developer/web/package.json", b"{}");
    h.big("Developer/web/node_modules/pkg/index.js");
    h.file("Developer/rs/Cargo.toml", b"[package]");
    h.big("Developer/rs/target/debug/app");
    h.big(".npm/_cacache/blob");
    h.big(".Trash/old.bin");
    h.big("Library/Caches/com.apple.Safari/x");
    h.big("Library/Caches/SomeApp/x");
    // A repo with a T3 Code worktree that has dependencies and an uncommitted file.
    h.file("Developer/repo/README.md", b"hi");
    h.git("Developer/repo", &["init", "-q"]);
    h.git("Developer/repo", &["add", "."]);
    h.git("Developer/repo", &["commit", "-qm", "init"]);
    fs::create_dir_all(h.0.join(".t3/worktrees/repo")).unwrap();
    h.git(
        "Developer/repo",
        &["worktree", "add", "-q", "-b", "feature", h.0.join(".t3/worktrees/repo/wt1").to_str().unwrap()],
    );
    h.file(".t3/worktrees/repo/wt1/package.json", b"{}");
    h.big(".t3/worktrees/repo/wt1/node_modules/dep/index.js");
    h.file(".t3/worktrees/repo/wt1/notes.txt", b"uncommitted");
    h
}

#[test]
fn scan_finds_entries_and_categories() {
    let h = fixture();
    let r = scan(&h.0);

    let nm = entry(&r, "node_modules", "Developer/web/node_modules");
    assert_eq!(nm.cat, Cat::Deps);
    assert_eq!(nm.parent, None);
    assert_eq!(entry(&r, "build", "Developer/rs/target").cat, Cat::Build);
    assert_eq!(entry(&r, "npm", ".npm/_cacache").cat, Cat::Caches);
    assert_eq!(entry(&r, "trash", ".Trash").cat, Cat::Trash);
    entry(&r, "appcaches", "Library/Caches/SomeApp");
    assert!(!r.entries.iter().any(|e| e.path.contains("com.apple.")), "Apple caches are left alone");

    let wt = entry(&r, "worktrees", ".t3/worktrees/repo/wt1");
    let info = wt.worktree.as_ref().expect("worktree inspected");
    assert_eq!(info.agent, Some("T3 Code"));
    assert_eq!(info.branch.as_deref(), Some("feature"));
    assert!(info.dirty >= 2, "notes.txt and package.json are untracked: {info:?}");
    assert!(info.merged, "a fresh branch is already part of main");
    assert!(!info.orphaned);
    assert_eq!(info.repo.as_deref(), Some("~/Developer/repo"));

    let inner = entry(&r, "node_modules", ".t3/worktrees/repo/wt1/node_modules");
    assert_eq!(inner.parent.as_deref(), Some(wt.key.as_str()), "dependencies inside a worktree point at it");
    assert_eq!(inner.cat, Cat::Worktrees, "and count towards the worktree in the bar");

    assert!(r.cats[&Cat::Worktrees] >= 1_200_000);
    assert!(r.cats[&Cat::Deps] >= 1_200_000);
    assert_eq!(r.scanned, r.cats.values().sum::<u64>(), "every scanned byte has exactly one category");

    let home = &r.tree.children[0];
    assert_eq!((home.name.as_str(), home.path.as_deref(), home.size), ("Home", Some(""), r.scanned));
}

#[test]
fn orphaned_worktree_is_reported() {
    let h = fixture();
    let record = h.0.join("Developer/repo/.git/worktrees/wt1");
    let aside = h.0.join("record-aside");
    // Tracked when scanned, and its repository's record gone by the time of the cleanup: git can
    // no longer be asked what is uncommitted, so the scan's answer is not acted on.
    let tracked = scan(&h.0);
    fs::rename(&record, &aside).unwrap();
    let wt = entry(&tracked, "worktrees", ".t3/worktrees/repo/wt1");
    let err = clean::clean_with(&h.0, wt, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("appeared or disappeared"), "{err}");

    let r = scan(&h.0);
    let orphan = entry(&r, "worktrees", ".t3/worktrees/repo/wt1");
    assert!(orphan.worktree.as_ref().unwrap().orphaned);
    // And the other way: shown as orphaned ("contents not checked"), then the record comes back.
    fs::rename(&aside, &record).unwrap();
    let err = clean::clean_with(&h.0, orphan, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("appeared or disappeared"), "{err}");
    assert!(h.0.join(".t3/worktrees/repo/wt1").exists());
}

#[test]
fn delete_removes_read_only_folders() {
    let h = fixture();
    let r = scan(&h.0);
    let e = entry(&r, "build", "Developer/rs/target");
    let ro = h.0.join("Developer/rs/target/debug");
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(clean::clean(&h.0, e).unwrap(), clean::Method::Delete);
    assert!(!h.0.join("Developer/rs/target").exists());
    assert!(h.0.join("Developer/rs/Cargo.toml").exists());
}

#[test]
fn empty_trash_keeps_the_folder() {
    let h = fixture();
    let r = scan(&h.0);
    assert_eq!(clean::clean(&h.0, entry(&r, "trash", ".Trash")).unwrap(), clean::Method::Empty);
    assert!(h.0.join(".Trash").is_dir());
    assert_eq!(fs::read_dir(h.0.join(".Trash")).unwrap().count(), 0);
}

#[test]
fn guard_refuses_unsafe_paths() {
    let h = TempHome::new("guard");
    h.file("Developer/x/file", b"x");
    std::os::unix::fs::symlink(h.0.join("Developer"), h.0.join("link")).unwrap();
    assert!(clean::guard(&h.0, &h.0).is_err(), "home itself");
    assert!(clean::guard(&h.0, Path::new("/etc/hosts")).is_err(), "outside home");
    assert!(clean::guard(&h.0, Path::new("/private/tmp")).is_err(), "a temp root itself");
    assert!(clean::guard(&h.0, Path::new("/Applications/Safari.app")).is_err(), "not owned by the user");
    assert!(clean::guard(&h.0, Path::new("/Applications/Xcode.app/Contents")).is_err(), "inside an app");
    assert!(clean::guard(&h.0, &h.0.join("Developer")).is_err(), "protected top-level folder");
    assert!(clean::guard(&h.0, &h.0.join("Developer/../Developer/x")).is_err(), "parent components");
    assert!(clean::guard(&h.0, &h.0.join("link")).is_err(), "symlink");
    assert!(clean::guard(&h.0, &h.0.join("Developer/x")).is_ok());
}

#[test]
fn project_build_folders_need_their_marker() {
    let h = TempHome::new("build");
    h.file("Developer/py/pyproject.toml", b"");
    h.big("Developer/py/.mypy_cache/3.12/cache.json");
    h.file("Developer/app/App.xcodeproj/project.pbxproj", b"");
    h.big("Developer/app/DerivedData/Build/app.o");
    h.file("Developer/java/pom.xml", b"<project/>");
    h.big("Developer/java/target/classes/App.class");
    // A folder that only shares a name with build output is left alone.
    h.file("Developer/notes/README.md", b"");
    h.big("Developer/notes/target/plan.txt");
    h.big("Developer/notes/build/draft.txt");
    let r = scan(&h.0);
    entry(&r, "build", "Developer/py/.mypy_cache");
    entry(&r, "build", "Developer/app/DerivedData");
    entry(&r, "build", "Developer/java/target");
    assert!(!r.entries.iter().any(|e| e.path.starts_with("Developer/notes")), "{:?}", r.entries);
}

#[test]
fn old_downloads_are_offered_and_new_ones_are_not() {
    let h = TempHome::new("downloads");
    h.file("Downloads/old.zip", &vec![1; 6_000_000]);
    h.age("Downloads/old.zip", 120);
    h.file("Downloads/new.zip", &vec![1; 6_000_000]);
    h.file("Downloads/old-small.txt", &vec![1; 1_200_000]);
    h.age("Downloads/old-small.txt", 120);
    h.file("Downloads/old-folder/data.bin", &vec![1; 6_000_000]);
    h.age("Downloads/old-folder/data.bin", 200);
    h.file("Downloads/busy-folder/old.bin", &vec![1; 6_000_000]);
    h.age("Downloads/busy-folder/old.bin", 200);
    h.file("Downloads/busy-folder/today.txt", b"still in use");
    let r = scan(&h.0);
    let old = entry(&r, "downloads", "Downloads/old.zip");
    assert!(old.idle_days.is_some_and(|d| (119..=121).contains(&d)), "{old:?}");
    assert!(entry(&r, "downloads", "Downloads/old-folder").idle_days.is_some_and(|d| d >= 199));
    let offered: Vec<_> = r.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(offered.len(), 2, "only the two old, large items: {offered:?}");
    // Review tier: both go to the Trash rather than being deleted. A file and a folder take
    // different routes through the "still untouched?" check, so each is cleared here.
    assert_eq!(clean::method(old), clean::Method::Trash);
    assert_eq!(clean::clean_with(&h.0, old, &trash_in(&h.0)).unwrap(), clean::Method::Trash);
    assert!(!h.0.join("Downloads/old.zip").exists() && h.0.join(".Trash/old.zip").exists());
    let folder = entry(&r, "downloads", "Downloads/old-folder");
    assert_eq!(clean::clean_with(&h.0, folder, &trash_in(&h.0)).unwrap(), clean::Method::Trash);
    assert!(!h.0.join("Downloads/old-folder").exists() && h.0.join(".Trash/old-folder/data.bin").exists());
    // What wasn't offered is untouched.
    assert!(h.0.join("Downloads/new.zip").exists() && h.0.join("Downloads/busy-folder/today.txt").exists());
}

#[test]
fn a_cancelled_scan_stops_early() {
    let h = fixture();
    let scanner = Scanner::new(&h.0).unwrap().home_only();
    scanner.cancel();
    let r = scanner.run();
    assert!(scanner.cancelled());
    assert_eq!((r.files, r.entries.len()), (0, 0), "nothing is walked once cancelled");
}

#[test]
fn delete_does_not_follow_links_out_of_an_entry() {
    let h = fixture();
    h.file("Documents/precious.txt", b"keep me");
    std::os::unix::fs::symlink(h.0.join("Documents"), h.0.join("Developer/rs/target/docs-link")).unwrap();
    let r = scan(&h.0);
    clean::clean(&h.0, entry(&r, "build", "Developer/rs/target")).unwrap();
    assert!(!h.0.join("Developer/rs/target").exists());
    assert_eq!(fs::read(h.0.join("Documents/precious.txt")).unwrap(), b"keep me");
}

/// A Trash inside the fake home, so tests never touch the real one.
fn trash_in(home: &Path) -> impl Fn(&Path) -> Result<(), String> {
    let trash = home.join(".Trash");
    move |p: &Path| {
        fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
        fs::rename(p, trash.join(p.file_name().unwrap())).map_err(|e| e.to_string())
    }
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repo with a detached worktree that has one commit no branch contains.
fn detached_fixture() -> TempHome {
    let h = TempHome::new("detached");
    h.file("Developer/repo/README.md", b"hi");
    h.git("Developer/repo", &["init", "-q"]);
    h.git("Developer/repo", &["add", "."]);
    h.git("Developer/repo", &["commit", "-qm", "init"]);
    fs::create_dir_all(h.0.join(".codex/worktrees/1")).unwrap();
    h.git(
        "Developer/repo",
        &["worktree", "add", "-q", "--detach", h.0.join(".codex/worktrees/1/repo").to_str().unwrap()],
    );
    h.big(".codex/worktrees/1/repo/work.bin");
    h.git(".codex/worktrees/1/repo", &["add", "."]);
    h.git(".codex/worktrees/1/repo", &["commit", "-qm", "only here"]);
    h
}

#[test]
fn trashing_a_detached_worktree_keeps_its_commits_reachable() {
    let h = detached_fixture();
    let r = scan(&h.0);
    let wt = entry(&r, "worktrees", ".codex/worktrees/1/repo");
    assert_eq!(wt.worktree.as_ref().unwrap().unique, 1);
    let sha = git_out(&h.0.join(".codex/worktrees/1/repo"), &["rev-parse", "HEAD"]);

    assert_eq!(clean::clean_with(&h.0, wt, &trash_in(&h.0)).unwrap(), clean::Method::Trash);
    assert!(!h.0.join(".codex/worktrees/1/repo").exists());
    let repo = h.0.join("Developer/repo");
    let refs = git_out(&repo, &["for-each-ref", "--contains", &sha, "--format=%(refname)"]);
    assert!(refs.contains("refs/storage/trashed/"), "a ref holds the commit: {refs:?}");
    assert!(!repo.join(".git/worktrees/repo").exists(), "git's record of that worktree is gone");
}

#[test]
fn a_worktree_git_cannot_inspect_is_unknown_not_clean() {
    let h = fixture();
    // git's record exists but is unreadable, as when git fails or times out.
    fs::write(h.0.join("Developer/repo/.git/worktrees/wt1/HEAD"), b"garbage").unwrap();
    let info = entry(&scan(&h.0), "worktrees", ".t3/worktrees/repo/wt1").worktree.clone().unwrap();
    assert!(info.unknown && !info.orphaned, "{info:?}");
    assert_eq!(info.dirty, 0, "nothing was counted, which is why `unknown` has to be checked first");

    // git answers again before the cleanup. What it has to say was never shown, so nothing goes.
    let r = scan(&h.0);
    let unseen = entry(&r, "worktrees", ".t3/worktrees/repo/wt1");
    h.git("Developer/repo", &["worktree", "repair"]);
    fs::write(h.0.join("Developer/repo/.git/worktrees/wt1/HEAD"), b"ref: refs/heads/feature\n").unwrap();
    let err = clean::clean_with(&h.0, unseen, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("can inspect it now"), "{err}");
    assert!(h.0.join(".t3/worktrees/repo/wt1").exists());
}

#[test]
fn work_added_after_the_scan_stops_the_cleanup() {
    let h = fixture();
    h.git(".t3/worktrees/repo/wt1", &["add", "."]);
    h.git(".t3/worktrees/repo/wt1", &["commit", "-qm", "wip"]);
    let r = scan(&h.0);
    let wt = entry(&r, "worktrees", ".t3/worktrees/repo/wt1");
    assert_eq!(wt.worktree.as_ref().unwrap().dirty, 0);
    h.file(".t3/worktrees/repo/wt1/new-idea.txt", b"written after the scan");
    let err = clean::clean_with(&h.0, wt, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("changed since the scan"), "{err}");
    assert!(h.0.join(".t3/worktrees/repo/wt1/new-idea.txt").exists());
}

#[test]
fn ignored_files_and_locks_are_reported() {
    let h = fixture();
    h.file(".t3/worktrees/repo/wt1/.gitignore", b".env\nnode_modules\n");
    h.file(".t3/worktrees/repo/wt1/.env", b"SECRET=1");
    h.git("Developer/repo", &["worktree", "lock", h.0.join(".t3/worktrees/repo/wt1").to_str().unwrap()]);
    let info = entry(&scan(&h.0), "worktrees", ".t3/worktrees/repo/wt1").worktree.clone().unwrap();
    assert!(info.locked);
    assert_eq!(
        (info.ignored, info.ignored_names.as_slice()),
        (1, &[".env".to_owned()][..]),
        "node_modules doesn't count"
    );
}

#[test]
fn state_that_looks_like_build_output_is_not_offered() {
    let h = TempHome::new("state");
    // Local D1/KV data lives in .wrangler/state; only .wrangler/tmp is disposable.
    h.file("Developer/worker/package.json", b"{}");
    h.big("Developer/worker/.wrangler/state/v3/d1/db.sqlite");
    h.big("Developer/worker/.wrangler/tmp/bundle.js");
    // A deploy folder with releases beside a `current` link is not an agent CLI's old versions.
    h.big("apps/shop/releases/old/app.bin");
    h.big("apps/shop/releases/new/app.bin");
    std::os::unix::fs::symlink(h.0.join("apps/shop/releases/new"), h.0.join("apps/shop/current")).unwrap();
    // The same layout inside an agent's worktree is still somebody's deploy folder.
    for agent in [".t3/worktrees/repo/wt1", ".claude/worktrees/task"] {
        h.big(&format!("{agent}/shop/releases/v1/app.bin"));
        h.big(&format!("{agent}/shop/releases/v2/app.bin"));
        std::os::unix::fs::symlink("releases/v2", h.0.join(format!("{agent}/shop/current"))).unwrap();
    }
    h.big(".codex/packages/standalone/releases/0.1/codex");
    h.big(".codex/packages/standalone/releases/0.2/codex");
    std::os::unix::fs::symlink("releases/0.2", h.0.join(".codex/packages/standalone/current")).unwrap();
    // Nothing inside a repository's own storage is a spare installer or large file.
    h.file("Developer/big/.git/objects/pack/pack-1.dmg", &vec![1; 21_000_000]);
    h.file("Documents/setup.dmg", &vec![1; 21_000_000]);
    let r = scan(&h.0);
    entry(&r, "build", "Developer/worker/.wrangler/tmp");
    entry(&r, "oldversions", ".codex/packages/standalone/releases/0.1");
    entry(&r, "installers", "Documents/setup.dmg");
    let paths: Vec<_> = r.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths.len(), 3, "and nothing else: {paths:?}");
}

#[test]
fn claude_memory_is_kept_when_transcripts_go() {
    let h = TempHome::new("memory");
    h.big(".claude/projects/-Users-x-app/session.jsonl");
    h.big(".claude/projects/-Users-x-app/memory/MEMORY.md");
    let r = scan(&h.0);
    let e = entry(&r, "agentlogs", ".claude/projects/-Users-x-app");
    assert_eq!(e.spare.as_deref(), Some("memory"));
    assert!(e.size < 2_000_000, "the memory folder isn't counted: {}", e.size);
    clean::clean_with(&h.0, e, &trash_in(&h.0)).unwrap();
    assert!(h.0.join(".claude/projects/-Users-x-app/memory/MEMORY.md").exists());
    assert!(!h.0.join(".claude/projects/-Users-x-app/session.jsonl").exists());
    let trashed: Vec<_> = fs::read_dir(h.0.join(".Trash")).unwrap().flatten().collect();
    assert_eq!(trashed.len(), 1, "the transcripts go as one folder that can be put back as one");
    assert!(trashed[0].path().join("session.jsonl").exists());
}

#[test]
fn a_failed_move_to_the_trash_puts_transcripts_back() {
    let h = TempHome::new("memory-fail");
    h.big(".claude/projects/-Users-x-app/session.jsonl");
    h.big(".claude/projects/-Users-x-app/other.jsonl");
    h.file(".claude/projects/-Users-x-app/memory/MEMORY.md", b"kept");
    let r = scan(&h.0);
    let e = entry(&r, "agentlogs", ".claude/projects/-Users-x-app");
    let err = clean::clean_with(&h.0, e, &|_| Err("the Trash said no".into())).unwrap_err();
    assert!(err.contains("the Trash said no"), "{err}");
    let project = h.0.join(".claude/projects/-Users-x-app");
    assert!(project.join("session.jsonl").exists() && project.join("other.jsonl").exists(), "back where they were");
    let siblings: Vec<_> =
        fs::read_dir(h.0.join(".claude/projects")).unwrap().flatten().map(|d| d.file_name()).collect();
    assert_eq!(siblings.len(), 1, "no holding folder left behind: {siblings:?}");

    // A Trash that says "done" without taking anything is a failure too.
    let err = clean::clean_with(&h.0, e, &|_| Ok(())).unwrap_err();
    assert!(err.contains("didn't take it"), "{err}");
    assert!(project.join("session.jsonl").exists() && project.join("other.jsonl").exists());

    // A session starts while the move is under way and writes a transcript under the old name.
    // Putting things back must not replace it: both stay, and the error says where the old one is.
    let err = clean::clean_with(&h.0, e, &|_| {
        fs::write(project.join("session.jsonl"), b"a new session").unwrap();
        Err("the Trash said no".into())
    })
    .unwrap_err();
    assert_eq!(fs::read(project.join("session.jsonl")).unwrap(), b"a new session");
    assert!(project.join("other.jsonl").exists(), "what could go back did");
    let holding =
        fs::read_dir(h.0.join(".claude/projects")).unwrap().flatten().map(|d| d.path()).find(|p| *p != project);
    let holding = holding.expect("the old transcript is kept in the holding folder");
    assert!(fs::metadata(holding.join("session.jsonl")).unwrap().len() > 1_000_000);
    assert!(err.contains(holding.to_str().unwrap()), "{err}");
}

#[test]
fn memory_written_after_the_scan_is_still_kept() {
    let h = TempHome::new("memory-late");
    h.big(".claude/projects/-Users-x-app/session.jsonl");
    let r = scan(&h.0);
    let e = entry(&r, "agentlogs", ".claude/projects/-Users-x-app");
    assert_eq!(e.spare.as_deref(), Some("memory"), "spared by rule, not because it had weight when scanned");
    h.file(".claude/projects/-Users-x-app/memory/MEMORY.md", b"learned after the scan");
    clean::clean_with(&h.0, e, &trash_in(&h.0)).unwrap();
    assert!(h.0.join(".claude/projects/-Users-x-app/memory/MEMORY.md").exists());
}

#[test]
fn ignored_local_state_counts_as_work() {
    let h = fixture();
    // A local D1 database under an ignored .wrangler is data, even though .wrangler/tmp is not.
    h.file(
        ".t3/worktrees/repo/wt1/.gitignore",
        b".wrangler
",
    );
    h.big(".t3/worktrees/repo/wt1/.wrangler/state/v3/d1/db.sqlite");
    let info = entry(&scan(&h.0), "worktrees", ".t3/worktrees/repo/wt1").worktree.clone().unwrap();
    assert!(info.ignored >= 1 && info.ignored_names.iter().any(|n| n.contains(".wrangler")), "{info:?}");
}

#[test]
fn cleanup_rechecks_what_the_scan_saw() {
    let h = TempHome::new("stale");
    h.file("Downloads/old-folder/data.bin", &vec![1; 6_000_000]);
    h.age("Downloads/old-folder/data.bin", 200);
    h.file("Downloads/old-folder/deep/data.bin", b"x");
    h.age("Downloads/old-folder/deep/data.bin", 200);
    h.file("Downloads/old.zip", &vec![1; 6_000_000]);
    h.age("Downloads/old.zip", 120);
    h.file("Developer/rs/Cargo.toml", b"[package]");
    h.big("Developer/rs/target/debug/app");
    h.file("Developer/other/Cargo.toml", b"[package]");
    h.big("Developer/other/target/precious");
    let r = scan(&h.0);
    let old = entry(&r, "downloads", "Downloads/old-folder");
    let old_file = entry(&r, "downloads", "Downloads/old.zip");
    let target = entry(&r, "build", "Developer/rs/target");

    // Something is written into a folder that was offered for being untouched.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    h.file("Downloads/old-folder/fresh.txt", b"new");
    let err = clean::clean_with(&h.0, old, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("written to"), "{err}");
    // The same for a file offered for being old, which is checked by its own time.
    fs::write(h.0.join("Downloads/old.zip"), vec![2; 6_000_000]).unwrap();
    let err = clean::clean_with(&h.0, old_file, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("written to"), "{err}");
    assert!(h.0.join("Downloads/old.zip").exists() && !h.0.join(".Trash/old.zip").exists());
    // And for a write to a file deeper in the folder, which leaves every folder's own time alone:
    // only looking at the files inside can tell. A new scan first, because adding and removing
    // `fresh.txt` has already moved the folder's own time past the old one.
    fs::remove_file(h.0.join("Downloads/old-folder/fresh.txt")).unwrap();
    let r1 = scan(&h.0);
    let old = entry(&r1, "downloads", "Downloads/old-folder");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(h.0.join("Downloads/old-folder/deep/data.bin"), b"y").unwrap();
    for folder in ["Downloads/old-folder", "Downloads/old-folder/deep"] {
        let at = std::os::unix::fs::MetadataExt::mtime(&fs::metadata(h.0.join(folder)).unwrap());
        assert!(at <= old.scanned, "{folder} itself wasn't touched");
    }
    let err = clean::clean_with(&h.0, old, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("written to"), "{err}");
    assert!(h.0.join("Downloads/old-folder/deep/data.bin").exists());
    fs::remove_file(h.0.join("Downloads/old-folder/deep/data.bin")).unwrap();

    // A folder inside it that can't be opened could hold anything, written at any time.
    h.file("Downloads/old-folder/inner/data.bin", b"x");
    h.age("Downloads/old-folder/inner/data.bin", 200);
    h.age("Downloads/old-folder/data.bin", 200);
    let r2 = scan(&h.0);
    let old = entry(&r2, "downloads", "Downloads/old-folder");
    fs::set_permissions(h.0.join("Downloads/old-folder/inner"), fs::Permissions::from_mode(0o000)).unwrap();
    let err = clean::clean_with(&h.0, old, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("can't be read") && !err.contains("scan again"), "{err}");
    fs::set_permissions(h.0.join("Downloads/old-folder/inner"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(h.0.join("Downloads/old-folder/data.bin").exists());

    // The project folder is swapped for a link to another project that also has a `target`.
    fs::rename(h.0.join("Developer/rs"), h.0.join("Developer/rs-moved")).unwrap();
    std::os::unix::fs::symlink(h.0.join("Developer/other"), h.0.join("Developer/rs")).unwrap();
    assert!(clean::guard(&h.0, &target.abs).is_err(), "reached through a symlink");
    assert!(clean::clean(&h.0, target).is_err());
    assert!(h.0.join("Developer/other/target/precious").exists());
}

#[test]
fn the_trash_is_emptied_before_anything_is_moved_into_it() {
    let h = fixture();
    let r = scan(&h.0);
    let mut batch =
        vec![entry(&r, "worktrees", ".t3/worktrees/repo/wt1").clone(), entry(&r, "trash", ".Trash").clone()];
    clean::plan(&mut batch);
    assert_eq!(batch[0].rec, "trash");
    for e in &batch {
        clean::clean_with(&h.0, e, &trash_in(&h.0)).unwrap();
    }
    assert!(!h.0.join(".Trash/old.bin").exists(), "what was already there is gone");
    assert!(h.0.join(".Trash/wt1/notes.txt").exists(), "what this cleanup trashed is still recoverable");

    // A Trash that is really a link somewhere else is refused.
    fs::remove_dir_all(h.0.join(".Trash")).unwrap();
    std::os::unix::fs::symlink(h.0.join("Developer"), h.0.join(".Trash")).unwrap();
    assert!(clean::clean(&h.0, &batch[0]).is_err());
    assert!(h.0.join("Developer/repo/README.md").exists());
}

#[test]
fn the_default_toolchain_is_held_back() {
    let h = TempHome::new("toolchain");
    h.file(".rustup/settings.toml", b"version = \"12\"\ndefault_toolchain = \"stable-aarch64-apple-darwin\"\n");
    h.big(".rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc");
    h.big(".rustup/toolchains/nightly-aarch64-apple-darwin/bin/rustc");
    let r = scan(&h.0);
    assert!(entry(&r, "toolchains", ".rustup/toolchains/stable-aarch64-apple-darwin").hold.is_some());
    let nightly = entry(&r, "toolchains", ".rustup/toolchains/nightly-aarch64-apple-darwin");
    assert!(nightly.hold.is_none());

    // `rustup default nightly` after the scan: it was free to go when it was offered, and isn't now.
    h.file(".rustup/settings.toml", b"default_toolchain = \"nightly-aarch64-apple-darwin\"\n");
    let err = clean::clean_with(&h.0, nightly, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("became the default version"), "{err}");
    assert!(h.0.join(".rustup/toolchains/nightly-aarch64-apple-darwin").exists());
}

/// Everything git could reach is expired and collected, so only real refs keep an object alive.
fn collect_garbage(repo: &Path) {
    git_out(repo, &["reflog", "expire", "--expire=now", "--all"]);
    git_out(repo, &["gc", "-q", "--prune=now"]);
}

fn object_exists(repo: &Path, id: &str) -> bool {
    Command::new("git").arg("-C").arg(repo).args(["cat-file", "-e", id]).status().unwrap().success()
}

#[test]
fn rescue_refs_never_overwrite_each_other() {
    let h = TempHome::new("collide");
    h.file("Developer/repo/README.md", b"hi");
    h.git("Developer/repo", &["init", "-q"]);
    h.git("Developer/repo", &["add", "."]);
    h.git("Developer/repo", &["commit", "-qm", "init"]);
    let mut shas = Vec::new();
    // Two names that differ only in a character ref names can't hold.
    for name in ["a_b", "a-b"] {
        let rel = format!(".codex/worktrees/{name}");
        h.git("Developer/repo", &["worktree", "add", "-q", "--detach", h.0.join(&rel).to_str().unwrap()]);
        h.big(&format!("{rel}/{name}.bin"));
        h.git(&rel, &["add", "."]);
        h.git(&rel, &["commit", "-qm", name]);
        shas.push(git_out(&h.0.join(&rel), &["rev-parse", "HEAD"]));
    }
    let r = scan(&h.0);
    for name in ["a_b", "a-b"] {
        clean::clean_with(&h.0, entry(&r, "worktrees", &format!(".codex/worktrees/{name}")), &trash_in(&h.0)).unwrap();
    }
    let repo = h.0.join("Developer/repo");
    collect_garbage(&repo);
    assert!(object_exists(&repo, &shas[0]) && object_exists(&repo, &shas[1]), "both commits survive a gc");

    // A detached worktree whose commit a branch already holds leaves no ref behind.
    let before = git_out(&repo, &["for-each-ref", "refs/storage"]).lines().count();
    h.git(
        "Developer/repo",
        &["worktree", "add", "-q", "--detach", h.0.join(".codex/worktrees/plain").to_str().unwrap()],
    );
    h.big(".codex/worktrees/plain/scratch.bin");
    let r = scan(&h.0);
    let plain = entry(&r, "worktrees", ".codex/worktrees/plain");
    clean::clean_with(&h.0, plain, &trash_in(&h.0)).unwrap();
    assert_eq!(git_out(&repo, &["for-each-ref", "refs/storage"]).lines().count(), before);
}

#[test]
fn a_worktree_with_changes_keeps_its_git_record() {
    let h = fixture();
    let wt = h.0.join(".t3/worktrees/repo/wt1");
    // Version A is staged, then the file is overwritten with version B: A exists only in the index.
    h.file(".t3/worktrees/repo/wt1/notes.txt", b"version A, staged");
    h.git(".t3/worktrees/repo/wt1", &["add", "notes.txt"]);
    let staged = git_out(&wt, &["rev-parse", ":notes.txt"]);
    h.file(".t3/worktrees/repo/wt1/notes.txt", b"version B, not staged");
    let r = scan(&h.0);
    clean::clean_with(&h.0, entry(&r, "worktrees", ".t3/worktrees/repo/wt1"), &trash_in(&h.0)).unwrap();

    let repo = h.0.join("Developer/repo");
    assert!(repo.join(".git/worktrees/wt1/index").exists(), "git's record, index included, is left alone");
    collect_garbage(&repo);
    assert!(object_exists(&repo, &staged), "the staged version survives a gc");
    assert!(!repo.join(".git/worktrees/wt1/locked").exists(), "saved as a commit, so no lock was needed");
    // Put back from the Trash, it is a working worktree again with both versions.
    fs::rename(h.0.join(".Trash/wt1"), &wt).unwrap();
    assert_eq!(git_out(&wt, &["show", ":notes.txt"]), "version A, staged");
    assert_eq!(fs::read(wt.join("notes.txt")).unwrap(), b"version B, not staged");

    // git prunes the record of a worktree whose folder is missing whenever it next tidies up, and
    // the index goes with it. The staged version was also saved as a commit, which outlives that.
    let saved = git_out(&repo, &["for-each-ref", "--format=%(refname)", "refs/storage/trashed"]);
    let saved = saved.lines().find(|r| r.contains("-staged-")).expect("a ref to what was staged").to_owned();
    fs::rename(&wt, h.0.join(".Trash/wt1")).unwrap();
    fs::remove_dir_all(repo.join(".git/worktrees/wt1")).unwrap();
    collect_garbage(&repo);
    assert_eq!(git_out(&repo, &["show", &format!("{saved}:notes.txt")]), "version A, staged");
}

#[test]
fn staged_work_that_cannot_be_saved_locks_the_record_only_if_the_move_happens() {
    let h = fixture();
    let rel = ".t3/worktrees/repo/wt1";
    let record = h.0.join("Developer/repo/.git/worktrees/wt1");
    h.git(rel, &["add", "notes.txt"]);
    // A lock left by a crashed git: nothing can be written through the index, so no commit of it.
    fs::write(record.join("index.lock"), b"").unwrap();
    let r = scan(&h.0);
    let wt = entry(&r, "worktrees", rel);
    // The move doesn't happen: git's record is exactly as it was, with no lock claiming otherwise.
    assert!(clean::clean_with(&h.0, wt, &|_| Ok(())).is_err());
    assert!(!record.join("locked").exists());
    // It does: the record, index included, is locked so git never prunes it.
    clean::clean_with(&h.0, wt, &trash_in(&h.0)).unwrap();
    assert!(record.join("locked").exists() && record.join("index").exists());
}

#[test]
fn a_worktree_that_changed_or_gained_company_is_left_alone() {
    let h = fixture();
    let rel = ".t3/worktrees/repo/wt1";
    h.git(rel, &["add", "."]);
    h.git(rel, &["commit", "-qm", "wip"]);
    h.file(&format!("{rel}/draft.txt"), b"first");
    let r = scan(&h.0);
    let wt = entry(&r, "worktrees", rel);

    // The same file edited again: the count of changed files is the same, the work is not.
    std::thread::sleep(std::time::Duration::from_millis(20));
    h.file(&format!("{rel}/draft.txt"), b"second, longer");
    let err = clean::clean_with(&h.0, wt, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("changed since the scan"), "{err}");

    // A new commit: nothing uncommitted before or after.
    fs::remove_file(h.0.join(rel).join("draft.txt")).unwrap();
    let r = scan(&h.0);
    let clean_wt = entry(&r, "worktrees", rel);
    h.file(&format!("{rel}/more.txt"), b"x");
    h.git(rel, &["add", "."]);
    h.git(rel, &["commit", "-qm", "after the scan"]);
    let err = clean::clean_with(&h.0, clean_wt, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("changed since the scan"), "{err}");

    // Edits that leave git's list of changed files exactly as it was: a name git would quote, a
    // file inside an untracked folder, an ignored file, and a different version staged.
    h.file(&format!("{rel}/.gitignore"), b"local.db\n");
    h.file(&format!("{rel}/caf\u{e9} notes.txt"), b"one");
    h.git(rel, &["add", "."]);
    h.git(rel, &["commit", "-qm", "names"]);
    h.file(&format!("{rel}/caf\u{e9} notes.txt"), b"two");
    h.file(&format!("{rel}/scratch/draft.txt"), b"one");
    h.file(&format!("{rel}/local.db"), b"one");
    h.file(&format!("{rel}/staged.txt"), b"staged one");
    h.git(rel, &["add", "staged.txt"]);
    h.file(&format!("{rel}/staged.txt"), b"working copy");
    let edits: [(&str, &dyn Fn()); 4] = [
        ("quoted name", &|| h.file(&format!("{rel}/caf\u{e9} notes.txt"), b"three, longer")),
        ("untracked folder", &|| h.file(&format!("{rel}/scratch/draft.txt"), b"two, longer")),
        ("ignored file", &|| h.file(&format!("{rel}/local.db"), b"two, longer")),
        ("staged version", &|| {
            // Another version goes into the index; the working copy and its flags stay as they were.
            let blob = Command::new("sh")
                .arg("-c")
                .arg("printf 'staged two' | git hash-object -w --stdin")
                .current_dir(h.0.join(rel))
                .output()
                .unwrap();
            let blob = String::from_utf8_lossy(&blob.stdout).trim().to_owned();
            h.git(rel, &["update-index", "--cacheinfo", &format!("100644,{blob},staged.txt")]);
        }),
    ];
    for (what, edit) in edits {
        let r = scan(&h.0);
        let seen = entry(&r, "worktrees", rel);
        std::thread::sleep(std::time::Duration::from_millis(20));
        edit();
        let err = clean::clean_with(&h.0, seen, &trash_in(&h.0)).expect_err(what);
        assert!(err.contains("changed since the scan"), "{what}: {err}");
        assert!(h.0.join(rel).exists(), "{what}");
    }

    // An ignored folder is one line in git's list. A file in it rewritten at the same length, in
    // the same second, is still a change.
    h.file(&format!("{rel}/.gitignore"), b"local.db\n.local\n");
    h.git(rel, &["add", ".gitignore"]);
    h.file(&format!("{rel}/.local/db.sqlite"), b"state one");
    let r = scan(&h.0);
    let seen = entry(&r, "worktrees", rel);
    h.file(&format!("{rel}/.local/db.sqlite"), b"state two");
    let err = clean::clean_with(&h.0, seen, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("changed since the scan"), "{err}");

    // Part of it can't be read: the look is incomplete, which is "not checked", never "unchanged".
    fs::create_dir_all(h.0.join(rel).join(".local/sealed")).unwrap();
    fs::set_permissions(h.0.join(rel).join(".local/sealed"), fs::Permissions::from_mode(0o000)).unwrap();
    let r = scan(&h.0);
    let seen = entry(&r, "worktrees", rel);
    assert!(seen.worktree.as_ref().unwrap().unknown, "an incomplete look is unknown");
    let err = clean::clean_with(&h.0, seen, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("unreadable or too large"), "{err}");
    fs::set_permissions(h.0.join(rel).join(".local/sealed"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(h.0.join(rel).join(".local")).unwrap();

    // A file that can be written but won't say what it is (an ACL denying its attributes): it
    // isn't "gone", and a look that can't see it isn't a complete look.
    h.file(&format!("{rel}/.local/db.sqlite"), b"state one");
    let sealed = h.0.join(rel).join(".local/db.sqlite");
    let me = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap();
    let acl = format!("{} deny readattr,readextattr,readsecurity", me.trim());
    assert!(Command::new("chmod").args(["+a", &acl]).arg(&sealed).status().unwrap().success());
    assert!(fs::symlink_metadata(&sealed).is_err(), "the fixture really is unreadable");
    let r = scan(&h.0);
    let seen = entry(&r, "worktrees", rel);
    let info = seen.worktree.as_ref().unwrap();
    assert!(info.partial && info.unknown, "{info:?}");
    let err = clean::clean_with(&h.0, seen, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("unreadable or too large"), "{err}");
    assert!(Command::new("chmod").arg("-N").arg(&sealed).status().unwrap().success());
    // The file becomes readable again and is rewritten. The scan never saw it, so that scan can't
    // vouch for anything, however complete a look is possible now.
    fs::write(&sealed, b"state three").unwrap();
    let err = clean::clean_with(&h.0, seen, &trash_in(&h.0)).unwrap_err();
    assert!(err.contains("unreadable or too large"), "{err}");
    assert!(h.0.join(rel).exists());
    fs::remove_dir_all(h.0.join(rel).join(".local")).unwrap();

    // The Trash moving something other than what was checked (here: nothing at all) is an error.
    let r = scan(&h.0);
    let err = clean::clean_with(&h.0, entry(&r, "worktrees", rel), &|_| Ok(())).unwrap_err();
    assert!(err.contains("something else"), "{err}");

    // A process that moved in after the scan, without touching a file. It is started through a
    // shell that exits at once, so it isn't a child of this process: Storage's own commands (the
    // `git` it runs in a worktree) don't count as someone working there.
    let r = scan(&h.0);
    let idle = entry(&r, "worktrees", rel);
    assert!(!idle.worktree.as_ref().unwrap().busy, "the scan's own git isn't someone working in it");
    let guest = Command::new("/bin/sh")
        .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
        .current_dir(h.0.join(rel))
        .output()
        .unwrap();
    let err = clean::clean_with(&h.0, idle, &trash_in(&h.0));
    // And one that was already there when scanned is shown as in use.
    let busy = scan(&h.0);
    let _ = Command::new("kill").arg(String::from_utf8_lossy(&guest.stdout).trim()).status();
    let err = err.unwrap_err();
    assert!(err.contains("running in it"), "{err}");
    assert!(entry(&busy, "worktrees", rel).worktree.as_ref().unwrap().busy);
    assert!(h.0.join(rel).exists());
}

#[test]
fn default_node_and_python_versions_are_held_back() {
    let h = TempHome::new("defaults");
    // nvm's default is an alias of an alias: default -> lts/* -> lts/jod -> v22.12.0
    h.file(".nvm/alias/default", b"lts/*\n");
    h.file(".nvm/alias/lts/*", b"lts/jod\n");
    h.file(".nvm/alias/lts/jod", b"v22.12.0\n");
    h.big(".nvm/versions/node/v22.12.0/bin/node");
    h.big(".nvm/versions/node/v24.0.0/bin/node");
    h.file(".pyenv/version", b"3.12.4\n");
    h.big(".pyenv/versions/3.12.4/bin/python");
    h.big(".pyenv/versions/3.9.1/bin/python");
    let r = scan(&h.0);
    let held = |path: &str| entry(&r, "toolchains", path).hold.is_some();
    assert!(
        held(".nvm/versions/node/v22.12.0") && !held(".nvm/versions/node/v24.0.0"),
        "the LTS default, not the newest"
    );
    assert!(held(".pyenv/versions/3.12.4") && !held(".pyenv/versions/3.9.1"));

    // An alias that leads nowhere readable: any version might be the default, so all are held.
    fs::write(h.0.join(".nvm/alias/default"), b"work\n").unwrap();
    let r = scan(&h.0);
    for v in ["v22.12.0", "v24.0.0"] {
        assert!(entry(&r, "toolchains", &format!(".nvm/versions/node/{v}")).hold.is_some());
    }
}

#[test]
fn a_simulator_goes_only_if_shut_down_and_unused_since_the_scan() {
    let h = TempHome::new("sim");
    let plist = |body: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>name</key><string>Test Phone</string>{body}</dict></plist>"#
        )
    };
    let dir = "Library/Developer/CoreSimulator/Devices/11111111-2222-3333-4444-555555555555";
    let write = |body: &str| h.file(&format!("{dir}/device.plist"), plist(body).as_bytes());
    let shut_down = "<key>state</key><integer>1</integer>";
    // Used long ago, as CoreSimulator records it.
    write(&format!("{shut_down}<key>lastUsedAt</key><date>2020-01-01T00:00:00Z</date>"));
    h.big(&format!("{dir}/data/disk.img"));
    let r = scan(&h.0);
    let mut sim = entry(&r, "simulators", dir).clone();
    assert_eq!(clean::method(&sim), clean::Method::Command);
    assert!(sim.idle_days.is_some_and(|d| d > 2000), "its age comes from lastUsedAt: {:?}", sim.idle_days);
    // The command is swapped for one that succeeds without deleting anything.
    sim.cmd = Some(vec!["true".into()]);

    let refused = |body: &str, why: &str| {
        write(body);
        let err = clean::clean(&h.0, &sim).unwrap_err();
        assert!(err.contains(why), "{err}");
    };
    refused("<key>state</key><integer>3</integer>", "booted");
    assert_eq!(entry(&scan(&h.0), "simulators", dir).hold.as_deref(), Some("booted"), "held back while booted");
    refused("<key>state</key><integer>4</integer>", "isn't shut down");
    refused("", "isn't shut down");
    // Booted and shut down again after the scan: shut down now, but not the idle device that was offered.
    refused(&format!("{shut_down}<key>lastUsedAt</key><date>2099-01-01T00:00:00Z</date>"), "used since the scan");
    h.file(&format!("{dir}/device.plist"), b"not a plist");
    assert!(clean::clean(&h.0, &sim).unwrap_err().contains("couldn't read"));

    write(&format!("{shut_down}<key>lastUsedAt</key><date>2020-01-01T00:00:00Z</date>"));
    assert_eq!(clean::clean(&h.0, &sim).unwrap(), clean::Method::Command);
}

/// The other half of every "refuse if it changed" check: nothing has changed, so nothing is
/// refused. One of each kind of thing the scan offers from a home folder, cleared straight after
/// the scan, the way the app does it.
#[test]
fn everything_offered_can_be_cleared_when_nothing_has_changed() {
    let h = fixture();
    h.file("Downloads/old.zip", &vec![1; 6_000_000]);
    h.age("Downloads/old.zip", 120);
    h.file("Downloads/old-folder/data.bin", &vec![1; 6_000_000]);
    h.age("Downloads/old-folder/data.bin", 200);
    h.file("Documents/setup.dmg", &vec![1; 21_000_000]);
    h.big("Library/Logs/SomeApp/app.log");
    h.big("Library/Application Support/SomeEditor/Cache/blob");
    h.big("Library/Containers/org.example.app/Data/Library/Caches/blob");
    h.big(".cache/sometool/blob");
    h.big(".cargo/registry/cache/crate.tar");
    h.big("Library/Developer/Xcode/DerivedData/App/build.o");
    h.big(".claude/projects/-Users-x-app/session.jsonl");
    h.file(".claude/projects/-Users-x-app/memory/MEMORY.md", b"kept");
    h.big(".claude/shell-snapshots/snap.sh");
    h.big("Documents/Codex/2026-01-01/task/out.bin");
    h.file("Developer/py/pyproject.toml", b"");
    h.file("Developer/py/.venv/pyvenv.cfg", b"home = /usr/bin");
    h.big("Developer/py/.venv/lib/site.py");
    h.big("Developer/py/.pytest_cache/v/cache");
    h.big(".rustup/toolchains/nightly-aarch64-apple-darwin/bin/rustc");
    h.big(".codex/packages/standalone/releases/0.1/codex");
    h.big(".codex/packages/standalone/releases/0.2/codex");
    std::os::unix::fs::symlink("releases/0.2", h.0.join(".codex/packages/standalone/current")).unwrap();

    let r = scan(&h.0);
    let mut batch: Vec<_> = r.entries.iter().filter(|e| e.parent.is_none()).cloned().collect();
    clean::plan(&mut batch);
    let recs: std::collections::BTreeSet<&str> = batch.iter().map(|e| e.rec).collect();
    for rec in [
        "trash",
        "worktrees",
        "node_modules",
        "build",
        "npm",
        "cargo",
        "xcode_dd",
        "appcaches",
        "electron",
        "containercaches",
        "toolcaches",
        "logs",
        "agentlogs",
        "agentcache",
        "codextasks",
        "venv",
        "toolchains",
        "oldversions",
        "downloads",
        "installers",
    ] {
        assert!(recs.contains(rec), "the fixture offers {rec}: {recs:?}");
    }
    for e in &batch {
        assert!(e.cmd.is_none(), "nothing here is removed by a tool: {}", e.key);
        let done = clean::clean_with(&h.0, e, &trash_in(&h.0));
        assert!(done.is_ok(), "{} was refused: {done:?}", e.key);
        let left = if e.rec == "trash" || e.spare.is_some() { true } else { !e.abs.exists() };
        assert!(left, "{} is still there", e.key);
    }
    assert!(h.0.join(".claude/projects/-Users-x-app/memory/MEMORY.md").exists());
}
