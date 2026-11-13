//! Removing a folder tree: fast, and without ever following a symlink.
//!
//! Every folder is opened relative to its parent's descriptor with `O_NOFOLLOW`, and everything in
//! it is unlinked relative to that descriptor, so a folder swapped for a link while the removal
//! runs can't redirect it somewhere else. Sibling subtrees near the top are removed in parallel,
//! which is two to three times faster than `rm -rf` on something like `node_modules`.
//! Read-only folders (Go's module cache) are made writable first, and a folder on another volume
//! (a disk image mounted inside the tree) is refused rather than emptied.

use rayon::prelude::*;
use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Once;

/// Levels below the root whose entries are removed in parallel. Deeper folders are small.
const PARALLEL_DEPTH: u32 = 4;
const OPEN_DIR: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

struct Dir(libc::c_int);

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: the descriptor is open and owned by this value.
        unsafe { libc::close(self.0) };
    }
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 { Err(io::Error::last_os_error()) } else { Ok(result) }
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidInput)
}

/// Each open folder on the way down holds a descriptor, and apps start with a limit of 256.
fn raise_fd_limit() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: `lim` is a valid out-parameter, and then a valid limit to set.
        unsafe {
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 {
                // macOS refuses anything above OPEN_MAX (10240), even when the hard limit says unlimited.
                let want = lim.rlim_max.min(10_240);
                if lim.rlim_cur < want {
                    lim.rlim_cur = want;
                    libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
                }
            }
        }
    });
}

/// Removes `path` and everything in it. A symlink is removed, never followed. Keeps going after
/// a failure so as much as possible is removed, and returns the first error.
pub fn remove(path: &Path) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).ok_or_else(invalid)?;
    // `file_name` is None for `..`, so the last component is always a real name.
    let name = CString::new(path.file_name().ok_or_else(invalid)?.as_bytes()).map_err(|_| invalid())?;
    let parent = CString::new(parent.as_os_str().as_bytes()).map_err(|_| invalid())?;
    // SAFETY: `parent` is a valid NUL-terminated path.
    let parent =
        Dir(check(unsafe { libc::open(parent.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) })?);
    raise_fd_limit();
    let st = stat_at(&parent, &name)?;
    let is_dir = st.st_mode & libc::S_IFMT == libc::S_IFDIR;
    remove_entry(&parent, &name, is_dir, st.st_dev, 0, None)
}

/// A target opened through the same symlink-refusing walk as [`remove_under`] and checked against
/// the identity the scan saw. Holding it open lets the caller ask afterwards where *that* object
/// is, when the move itself has to be done by path (the Trash).
pub struct Held(Dir);

impl Held {
    /// Where the held object is now.
    pub fn path(&self) -> Option<std::path::PathBuf> {
        let mut buf = [0u8; libc::PATH_MAX as usize];
        // SAFETY: the descriptor is open and the buffer is PATH_MAX bytes, as F_GETPATH requires.
        if unsafe { libc::fcntl(self.0.0, libc::F_GETPATH, buf.as_mut_ptr()) } < 0 {
            return None;
        }
        let len = buf.iter().position(|b| *b == 0)?;
        Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
    }
}

/// Opens `path` (a folder or a regular file below `root`) for [`Held`]. `Ok(None)` when it is
/// something that can't be held open safely (a pipe, a device, an unreadable file).
pub fn hold(root: &Path, path: &Path, identity: Option<(u64, u64)>) -> io::Result<Option<Held>> {
    let rel = path.strip_prefix(root).map_err(|_| invalid())?;
    let name = cstr(rel.file_name().ok_or_else(invalid)?)?;
    let parent = descend(root, rel.parent().unwrap_or(Path::new("")))?;
    let st = stat_at(&parent, &name)?;
    let kind = st.st_mode & libc::S_IFMT;
    if kind != libc::S_IFDIR && kind != libc::S_IFREG {
        return Ok(None);
    }
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    // SAFETY: `parent` is open and `name` is NUL-terminated.
    let Ok(fd) = check(unsafe { libc::openat(parent.0, name.as_ptr(), flags) }) else { return Ok(None) };
    let held = Held(Dir(fd));
    // SAFETY: stat is plain data and the descriptor is open.
    let now = unsafe {
        let mut now: libc::stat = std::mem::zeroed();
        check(libc::fstat(fd, &mut now))?;
        now
    };
    if identity.is_some_and(|(dev, ino)| (now.st_dev as u64, now.st_ino) != (dev, ino)) {
        return Err(replaced());
    }
    Ok(Some(held))
}

fn replaced() -> io::Error {
    io::Error::other("was replaced since the scan")
}

fn cstr(s: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| invalid())
}

/// Opens the folder `rel` inside `root`, refusing to pass through a symlink at any step below
/// `root`. `rel` must be plain names (no `..`).
fn descend(root: &Path, rel: &Path) -> io::Result<Dir> {
    let root_c = cstr(root.as_os_str())?;
    // SAFETY: `root_c` is a valid NUL-terminated path.
    let mut dir =
        Dir(check(unsafe { libc::open(root_c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) })?);
    for part in rel.components() {
        let std::path::Component::Normal(name) = part else { return Err(invalid()) };
        let name = cstr(name)?;
        // SAFETY: `dir` is open and `name` is NUL-terminated.
        dir = Dir(check(unsafe { libc::openat(dir.0, name.as_ptr(), OPEN_DIR) })?);
    }
    Ok(dir)
}

/// Like [`remove`], for `path` somewhere below `root`: every folder between the two is opened
/// with `O_NOFOLLOW`, so a parent swapped for a symlink after the scan is refused, not followed.
/// `identity` is the device and inode the scan saw; anything else at that path is left alone.
pub fn remove_under(root: &Path, path: &Path, identity: Option<(u64, u64)>) -> io::Result<()> {
    let rel = path.strip_prefix(root).map_err(|_| invalid())?;
    let name = cstr(rel.file_name().ok_or_else(invalid)?)?;
    let parent = descend(root, rel.parent().unwrap_or(Path::new("")))?;
    raise_fd_limit();
    let st = stat_at(&parent, &name)?;
    if identity.is_some_and(|(dev, ino)| (st.st_dev as u64, st.st_ino) != (dev, ino)) {
        return Err(replaced());
    }
    let is_dir = st.st_mode & libc::S_IFMT == libc::S_IFDIR;
    // A folder is checked again on the descriptor it is emptied through: the name could be
    // pointed at something else between the look above and the open.
    remove_entry(&parent, &name, is_dir, st.st_dev, 0, identity.map(|(_, ino)| ino))
}

/// Removes everything inside the folder `rel` of `root` and keeps the folder. Refuses if `rel` is
/// (or is reached through) a symlink.
pub fn empty_under(root: &Path, rel: &Path) -> io::Result<()> {
    let dir = descend(root, rel)?;
    raise_fd_limit();
    // SAFETY: stat is plain data and `dir` is open.
    let dev = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        check(libc::fstat(dir.0, &mut st))?;
        st.st_dev
    };
    let mut first = Ok(());
    for (name, is_dir) in read_names(&dir)? {
        let is_dir = match is_dir {
            Some(d) => Ok(d),
            None => stat_at(&dir, &name).map(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR),
        };
        let result = is_dir.and_then(|d| remove_entry(&dir, &name, d, dev, 0, None));
        first = first.and(result);
    }
    first
}

fn stat_at(dir: &Dir, name: &CStr) -> io::Result<libc::stat> {
    // SAFETY: stat is plain data, `dir` is open and `name` is NUL-terminated.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        check(libc::fstatat(dir.0, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW))?;
        Ok(st)
    }
}

/// `expect`: the inode this entry must have, for the one the caller was asked to remove.
fn remove_entry(
    parent: &Dir,
    name: &CStr,
    is_dir: bool,
    dev: libc::dev_t,
    depth: u32,
    expect: Option<u64>,
) -> io::Result<()> {
    if !is_dir {
        // SAFETY: `parent` is open and `name` is NUL-terminated. Unlinking a symlink removes the link.
        return check(unsafe { libc::unlinkat(parent.0, name.as_ptr(), 0) }).map(drop);
    }
    let dir = open_dir(parent, name)?;
    // SAFETY: stat is plain data and `dir` is open.
    let st = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        check(libc::fstat(dir.0, &mut st))?;
        st
    };
    if st.st_dev != dev {
        return Err(io::Error::other("is on another volume; eject it first"));
    }
    if expect.is_some_and(|ino| st.st_ino != ino) {
        return Err(replaced());
    }
    let mode = st.st_mode & 0o7777;
    if mode & 0o700 != 0o700 {
        // SAFETY: `dir` is open. Failing is fine: the unlinks below report what couldn't be removed.
        unsafe { libc::fchmod(dir.0, mode | 0o700) };
    }
    let entries = read_names(&dir)?;
    let one = |(child, child_is_dir): &(CString, Option<bool>)| -> io::Result<()> {
        let child_is_dir = match child_is_dir {
            Some(d) => *d,
            None => stat_at(&dir, child)?.st_mode & libc::S_IFMT == libc::S_IFDIR,
        };
        remove_entry(&dir, child, child_is_dir, dev, depth + 1, None)
    };
    let emptied = if depth < PARALLEL_DEPTH && entries.len() > 1 {
        entries.par_iter().map(one).reduce(|| Ok(()), |a, b| a.and(b))
    } else {
        // Not `try_for_each`: one failure shouldn't leave the rest of the folder behind.
        let mut first = Ok(());
        for entry in &entries {
            let result = one(entry);
            first = first.and(result);
        }
        first
    };
    drop(dir);
    emptied?;
    // The name must still be the folder that was emptied before the (now empty) folder goes.
    if expect.is_some_and(|ino| stat_at(parent, name).map_or(true, |st| st.st_ino != ino)) {
        return Err(replaced());
    }
    // SAFETY: `parent` is open and `name` is NUL-terminated.
    check(unsafe { libc::unlinkat(parent.0, name.as_ptr(), libc::AT_REMOVEDIR) }).map(drop)
}

/// Opens a folder inside `parent`, making it accessible first if its owner locked themselves out.
fn open_dir(parent: &Dir, name: &CStr) -> io::Result<Dir> {
    // SAFETY (both calls): `parent` is open and `name` is NUL-terminated.
    let open = || check(unsafe { libc::openat(parent.0, name.as_ptr(), OPEN_DIR) }).map(Dir);
    open().or_else(|e| {
        if e.raw_os_error() != Some(libc::EACCES) {
            return Err(e);
        }
        unsafe { libc::fchmodat(parent.0, name.as_ptr(), 0o700, libc::AT_SYMLINK_NOFOLLOW) };
        open()
    })
}

/// Names in a folder, with whether each is a folder when the file system says so.
fn read_names(dir: &Dir) -> io::Result<Vec<(CString, Option<bool>)>> {
    // `fdopendir` takes over the descriptor it's given, so hand it a duplicate.
    // SAFETY: `dir` is open; the stream owns the duplicate and is closed below.
    let stream = unsafe {
        let dup = check(libc::fcntl(dir.0, libc::F_DUPFD_CLOEXEC, 0))?;
        let stream = libc::fdopendir(dup);
        if stream.is_null() {
            let err = io::Error::last_os_error();
            libc::close(dup);
            return Err(err);
        }
        stream
    };
    let mut names = Vec::new();
    let result = loop {
        // SAFETY: `stream` is open; errno is cleared to tell the end of the folder from an error.
        let entry = unsafe {
            *libc::__error() = 0;
            libc::readdir(stream)
        };
        if entry.is_null() {
            let err = io::Error::last_os_error();
            break if err.raw_os_error() == Some(0) { Ok(names) } else { Err(err) };
        }
        // SAFETY: `readdir` returned a valid entry whose name is NUL-terminated.
        let (name, kind) = unsafe { (CStr::from_ptr((*entry).d_name.as_ptr()), (*entry).d_type) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let is_dir = match kind {
            libc::DT_UNKNOWN => None,
            libc::DT_DIR => Some(true),
            _ => Some(false),
        };
        names.push((name.to_owned(), is_dir));
    };
    // SAFETY: `stream` is open and not used again.
    unsafe { libc::closedir(stream) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("storage-remove-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn removes_a_tree_without_following_links() {
        let root = temp("tree");
        let keep = root.join("keep");
        fs::create_dir_all(keep.join("inner")).unwrap();
        fs::write(keep.join("inner/precious"), b"x").unwrap();

        let tree = root.join("tree");
        for a in 0..12 {
            for b in 0..12 {
                let d = tree.join(format!("pkg{a}/lib{b}/deep/deeper/deepest"));
                fs::create_dir_all(&d).unwrap();
                fs::write(d.join("file.js"), b"data").unwrap();
                fs::write(d.parent().unwrap().join("other"), b"data").unwrap();
            }
        }
        symlink(&keep, tree.join("link-to-folder")).unwrap();
        symlink(keep.join("inner/precious"), tree.join("pkg0/link-to-file")).unwrap();
        symlink("/nonexistent", tree.join("pkg1/dangling")).unwrap();
        // A name that isn't valid UTF-8 has to be removed by its exact bytes.
        fs::write(tree.join(std::ffi::OsString::from_vec(b"odd-\xff-name".to_vec())), b"x").unwrap_or(());
        // Read-only and unreadable folders, like Go's module cache.
        fs::set_permissions(tree.join("pkg2/lib0/deep"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(tree.join("pkg3"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(tree.join("pkg4/lib1"), fs::Permissions::from_mode(0o000)).unwrap();

        remove(&tree).unwrap();
        assert!(!tree.exists());
        assert!(keep.join("inner/precious").exists(), "links are removed, not followed");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_link_at_the_root_is_only_unlinked() {
        let root = temp("link");
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/file"), b"x").unwrap();
        symlink(root.join("real"), root.join("link")).unwrap();
        remove(&root.join("link")).unwrap();
        assert!(fs::symlink_metadata(root.join("link")).is_err());
        assert!(root.join("real/file").exists());

        fs::write(root.join("plain"), b"x").unwrap();
        remove(&root.join("plain")).unwrap();
        assert!(!root.join("plain").exists());
        assert!(remove(&root.join("missing")).is_err());
        assert!(remove(Path::new("/")).is_err());
        assert!(remove(&root.join("real/..")).is_err(), "no parent components");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_swapped_parent_or_target_is_refused() {
        let root = temp("under");
        fs::create_dir_all(root.join("proj/target")).unwrap();
        fs::write(root.join("proj/target/a.o"), b"x").unwrap();
        fs::create_dir_all(root.join("elsewhere/target")).unwrap();
        fs::write(root.join("elsewhere/target/precious"), b"x").unwrap();
        use std::os::unix::fs::MetadataExt;
        let md = fs::metadata(root.join("proj/target")).unwrap();
        let seen = Some((md.dev(), md.ino()));

        // The parent becomes a link to another folder that also has a `target`.
        fs::rename(root.join("proj"), root.join("proj-moved")).unwrap();
        symlink(root.join("elsewhere"), root.join("proj")).unwrap();
        assert!(remove_under(&root, &root.join("proj/target"), seen).is_err());
        assert!(root.join("elsewhere/target/precious").exists());

        // The same path now holds a different folder.
        fs::remove_file(root.join("proj")).unwrap();
        fs::create_dir_all(root.join("proj/target")).unwrap();
        fs::write(root.join("proj/target/new"), b"x").unwrap();
        let err = remove_under(&root, &root.join("proj/target"), seen).unwrap_err();
        assert!(err.to_string().contains("replaced"), "{err}");
        assert!(root.join("proj/target/new").exists());

        // The check that matters is on the folder actually opened: a name swapped between the
        // look and the open leads to a folder with another inode, which must be left alone.
        let parent = descend(&root, Path::new("proj")).unwrap();
        let st = stat_at(&parent, c"target").unwrap();
        let wrong = Some(st.st_ino + 1);
        let err = remove_entry(&parent, c"target", true, st.st_dev, 0, wrong).unwrap_err();
        assert!(err.to_string().contains("replaced"), "{err}");
        assert!(root.join("proj/target/new").exists(), "nothing inside was touched");

        // Unchanged: removed.
        assert!(remove_under(&root, &root.join("proj-moved/target"), seen).is_ok());
        assert!(!root.join("proj-moved/target").exists());

        // Emptying a folder that is really a link is refused.
        symlink(root.join("elsewhere"), root.join("trash")).unwrap();
        assert!(empty_under(&root, Path::new("trash")).is_err());
        assert!(root.join("elsewhere/target/precious").exists());
        empty_under(&root, Path::new("elsewhere")).unwrap();
        assert!(root.join("elsewhere").is_dir() && !root.join("elsewhere/target").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
