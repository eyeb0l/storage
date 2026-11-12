//! Small macOS helpers: disk capacity, running commands with a time limit.

use serde::Serialize;
use std::ffi::{CString, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Disk {
    pub total: u64,
    pub free: u64,
}

/// Capacity of the volume holding `path`. `free` is what's available to the user, without purgeable space.
pub fn disk(path: &Path) -> Disk {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return Disk::default() };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a properly sized out-parameter.
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return Disk::default();
    }
    let bsize = s.f_bsize as u64;
    Disk { total: s.f_blocks as u64 * bsize, free: s.f_bavail as u64 * bsize }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn computer_name() -> String {
    let mut cmd = Command::new("scutil");
    cmd.args(["--get", "ComputerName"]);
    run(cmd, Some(Duration::from_secs(5))).map(|o| o.stdout.trim().to_owned()).unwrap_or_default()
}

/// Apps launched from Finder get a minimal PATH; add the usual tool locations so `git`,
/// `docker`, `ollama` and `xcrun` resolve the same way they do in a terminal.
static PATH_ENV: LazyLock<OsString> = LazyLock::new(|| {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let home = home();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.push(home.join(".orbstack/bin"));
    dirs.push(PathBuf::from("/Applications/Docker.app/Contents/Resources/bin"));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    std::env::join_paths(dirs).unwrap_or_default()
});

pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `cmd` to completion, killing it after `timeout`. Returns `None` if it couldn't start or timed out.
pub fn run(mut cmd: Command, timeout: Option<Duration>) -> Option<Output> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("PATH", &*PATH_ENV);
    let mut child = cmd.spawn().ok()?;
    let drain = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    };
    let out = drain(Box::new(child.stdout.take()?));
    let err = drain(Box::new(child.stderr.take()?));
    let deadline = timeout.map(|t| Instant::now() + t);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if deadline.is_some_and(|d| Instant::now() >= d) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let status = status?;
    Some(Output {
        ok: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn mtime(path: &Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).map(|m| m.mtime()).unwrap_or(0)
}

pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and can't fail.
    unsafe { libc::getuid() }
}

/// A per-user folder under /var/folders (`DARWIN_USER_TEMP_DIR` or `DARWIN_USER_CACHE_DIR`), canonical and
/// without a trailing slash.
pub fn darwin_dir(name: &str) -> Option<PathBuf> {
    let mut cmd = Command::new("getconf");
    cmd.arg(name);
    let out = run(cmd, Some(Duration::from_secs(5))).filter(|o| o.ok)?;
    std::fs::canonicalize(out.stdout.trim()).ok()
}

/// The Xcode that `xcode-select` points to (`/Applications/Xcode.app`), if it's a full Xcode and not
/// just the Command Line Tools. Uses `xcode-select`, which never shows the "install tools" dialog.
pub fn xcode_app() -> Option<PathBuf> {
    let dev = std::env::var_os("DEVELOPER_DIR").map(PathBuf::from).or_else(|| {
        let mut cmd = Command::new("/usr/bin/xcode-select");
        cmd.arg("-p");
        run(cmd, Some(Duration::from_secs(5))).filter(|o| o.ok).map(|o| PathBuf::from(o.stdout.trim()))
    })?;
    let app = dev.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
    std::fs::canonicalize(app).ok()
}
