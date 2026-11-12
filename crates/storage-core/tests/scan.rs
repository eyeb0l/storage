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
    fs::remove_dir_all(h.0.join("Developer/repo/.git/worktrees/wt1")).unwrap();
    let r = scan(&h.0);
    let info = entry(&r, "worktrees", ".t3/worktrees/repo/wt1").worktree.clone().unwrap();
    assert!(info.orphaned);
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
