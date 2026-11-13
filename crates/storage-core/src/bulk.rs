//! Listing a folder with everything the scan needs in one system call per few hundred entries.
//!
//! `readdir` plus an `lstat` per file is what makes a million-file walk slow: nearly all the time
//! goes to the kernel, one round trip per file. `getattrlistbulk(2)` returns names, types, sizes,
//! link counts, owners and modification times for a whole batch at once.

use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    /// Sockets, pipes, devices.
    Other,
}

/// One folder entry. Never follows symlinks: a link is reported as a link.
#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    /// Hard links to a file; 1 for anything else.
    pub nlink: u32,
    /// Bytes allocated on disk, for files; 0 for anything else.
    pub size: u64,
    pub mtime: i64,
    pub uid: u32,
}

const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_OWNERID: u32 = 0x0000_8000;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

const BUF_SIZE: usize = 128 * 1024;

thread_local! {
    // u64s so the buffer is aligned for the entry headers.
    static BUF: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Lists `dir`. With `follow` false, `dir` itself must not be a symlink (the walk only descends
/// into real folders, so a folder swapped for a link mid-scan is refused rather than followed).
pub fn list(dir: &Path, follow: bool) -> io::Result<Vec<Item>> {
    list_counting(dir, follow).map(|(items, _)| items)
}

/// [`list`], plus how many entries were left out because the system wouldn't describe them.
/// A walk that adds things up can ignore those; one that must prove nothing changed can't.
pub fn list_counting(dir: &Path, follow: bool) -> io::Result<(Vec<Item>, usize)> {
    match list_bulk(dir, follow) {
        // Some file systems (network shares, FUSE) don't support the bulk call.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) => list_stat(dir),
        other => other,
    }
}

struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: the descriptor is open and owned by this value.
        unsafe { libc::close(self.0) };
    }
}

fn list_bulk(dir: &Path, follow: bool) -> io::Result<(Vec<Item>, usize)> {
    let c = CString::new(dir.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | if follow { 0 } else { libc::O_NOFOLLOW };
    // SAFETY: `c` is a valid NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = Fd(fd);
    // SAFETY: attrlist is plain data; all-zero is a valid value.
    let mut want: libc::attrlist = unsafe { std::mem::zeroed() };
    want.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    want.commonattr = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_DEVID
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_MODTIME
        | ATTR_CMN_OWNERID
        | ATTR_CMN_FILEID
        | ATTR_CMN_ERROR;
    want.fileattr = ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE;

    BUF.with_borrow_mut(|buf| {
        buf.resize(BUF_SIZE / 8, 0);
        let mut items = Vec::new();
        let mut skipped = 0;
        loop {
            // SAFETY: `fd` is an open folder, `want` outlives the call and the buffer is BUF_SIZE bytes.
            let n =
                unsafe { libc::getattrlistbulk(fd.0, (&raw mut want).cast(), buf.as_mut_ptr().cast(), BUF_SIZE, 0) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if n == 0 {
                return Ok((items, skipped));
            }
            // SAFETY: the kernel filled the buffer; reinterpreting initialised u64s as bytes is always valid.
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), BUF_SIZE) };
            let mut at = 0;
            for i in 0..n as usize {
                // A record that doesn't fit its own length can't be stepped over: everything after
                // it in this batch is lost, and counted as such.
                let entry = read_u32(bytes, at)
                    .map(|l| l as usize)
                    .filter(|len| *len >= 24)
                    .and_then(|len| bytes.get(at..at + len));
                let Some(entry) = entry else {
                    skipped += n as usize - i;
                    break;
                };
                match parse(entry) {
                    Some(item) => items.push(item),
                    None => skipped += 1,
                }
                at += entry.len();
            }
        }
    })
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// One entry: its length, the set of attributes actually returned, then those attributes in a
/// fixed order (the error first, then each group in bit order). Anything that doesn't parse, or
/// that the kernel couldn't read, is skipped, like a failed `lstat` would be.
fn parse(e: &[u8]) -> Option<Item> {
    let common = read_u32(e, 4)?;
    let file = read_u32(e, 16)?;
    let mut at = 24;
    if common & ATTR_CMN_ERROR != 0 {
        if read_u32(e, at)? != 0 {
            return None;
        }
        at += 4;
    }
    if common & ATTR_CMN_NAME == 0 {
        return None;
    }
    // The name is stored at an offset from this reference, NUL-terminated.
    let offset = read_u32(e, at)? as i32;
    let len = read_u32(e, at + 4)? as usize;
    let start = usize::try_from(at as i64 + i64::from(offset)).ok()?;
    let raw = e.get(start..start + len.checked_sub(1)?)?;
    let name = OsStr::from_bytes(raw).to_string_lossy().into_owned();
    at += 8;
    let mut dev = 0;
    if common & ATTR_CMN_DEVID != 0 {
        // dev_t is a signed 32-bit value; widen it the way `MetadataExt::dev` does.
        dev = read_u32(e, at)? as i32 as u64;
        at += 4;
    }
    let mut kind = Kind::Other;
    if common & ATTR_CMN_OBJTYPE != 0 {
        kind = match read_u32(e, at)? {
            VREG => Kind::File,
            VDIR => Kind::Dir,
            VLNK => Kind::Symlink,
            _ => Kind::Other,
        };
        at += 4;
    }
    let mut mtime = 0;
    if common & ATTR_CMN_MODTIME != 0 {
        mtime = read_u64(e, at)? as i64;
        at += 16;
    }
    let mut uid = 0;
    if common & ATTR_CMN_OWNERID != 0 {
        uid = read_u32(e, at)?;
        at += 4;
    }
    let mut ino = 0;
    if common & ATTR_CMN_FILEID != 0 {
        ino = read_u64(e, at)?;
        at += 8;
    }
    let mut nlink = 1;
    if file & ATTR_FILE_LINKCOUNT != 0 {
        nlink = read_u32(e, at)?;
        at += 4;
    }
    let mut size = 0;
    if file & ATTR_FILE_ALLOCSIZE != 0 {
        size = read_u64(e, at)?;
    }
    if kind != Kind::File {
        (nlink, size) = (1, 0);
    }
    Some(Item { name, kind, dev, ino, nlink, size, mtime, uid })
}

/// The portable way: one `lstat` per entry.
fn list_stat(dir: &Path) -> io::Result<(Vec<Item>, usize)> {
    let mut skipped = 0;
    let items = fs::read_dir(dir)?
        .filter_map(|e| {
            // DirEntry::metadata doesn't follow symlinks.
            let found = e.ok().and_then(|e| Some((e.file_name(), e.metadata().ok()?)));
            let Some((name, md)) = found else {
                skipped += 1;
                return None;
            };
            let ft = md.file_type();
            let kind = if ft.is_dir() {
                Kind::Dir
            } else if ft.is_file() {
                Kind::File
            } else if ft.is_symlink() {
                Kind::Symlink
            } else {
                Kind::Other
            };
            let file = kind == Kind::File;
            Some(Item {
                name: name.to_string_lossy().into_owned(),
                kind,
                dev: md.dev(),
                ino: md.ino(),
                nlink: if file { md.nlink() as u32 } else { 1 },
                size: if file { md.blocks() * 512 } else { 0 },
                mtime: md.mtime(),
                uid: md.uid(),
            })
        })
        .collect();
    Ok((items, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bulk call has to agree with `lstat` on everything the scan uses.
    #[test]
    fn bulk_matches_lstat() {
        let dir = std::env::temp_dir().join(format!("storage-bulk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub/inner")).unwrap();
        fs::write(dir.join("empty"), b"").unwrap();
        fs::write(dir.join("small.txt"), b"hello").unwrap();
        fs::write(dir.join("big.bin"), vec![7u8; 300_000]).unwrap();
        fs::write(dir.join("naïve name with spaces"), b"x").unwrap();
        fs::hard_link(dir.join("big.bin"), dir.join("big-link.bin")).unwrap();
        std::os::unix::fs::symlink("sub", dir.join("to-sub")).unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.join("dangling")).unwrap();
        for i in 0..2500 {
            fs::write(dir.join("sub").join(format!("file-with-a-fairly-long-name-{i:05}.txt")), b"data").unwrap();
        }

        for d in [dir.clone(), dir.join("sub")] {
            let key = |i: &Item| i.name.clone();
            let (mut fast, _) = list_bulk(&d, false).unwrap();
            let (mut slow, _) = list_stat(&d).unwrap();
            fast.sort_by_key(key);
            slow.sort_by_key(key);
            assert_eq!(fast.len(), slow.len(), "same entries in {}", d.display());
            for (f, s) in fast.iter().zip(&slow) {
                assert_eq!(
                    (&f.name, f.kind, f.dev, f.ino, f.nlink, f.size, f.mtime, f.uid),
                    (&s.name, s.kind, s.dev, s.ino, s.nlink, s.size, s.mtime, s.uid)
                );
            }
        }
        let big = list(&dir, false).unwrap().into_iter().find(|i| i.name == "big.bin").unwrap();
        assert_eq!((big.kind, big.nlink), (Kind::File, 2));
        assert!(big.size >= 300_000);

        assert!(list(&dir.join("to-sub"), false).is_err(), "a symlink is refused unless asked to follow");
        assert_eq!(list(&dir.join("to-sub"), true).unwrap().len(), 2501);
        assert!(list(&dir.join("small.txt"), true).is_err(), "not a folder");
        fs::remove_dir_all(&dir).unwrap();
    }
}
