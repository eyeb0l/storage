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

/// Runs `cmd` to completion, killing it and anything it started after `timeout`. Returns `None`
/// if it couldn't start or timed out. The limit covers reading its output too: a helper it left
/// behind holding the pipe open can't keep the caller waiting.
pub fn run(mut cmd: Command, timeout: Option<Duration>) -> Option<Output> {
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc;
    // Its own process group, so a timeout can take the helpers it spawned down with it.
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("PATH", &*PATH_ENV).process_group(0);
    let mut child = cmd.spawn().ok()?;
    let group = -(child.id() as libc::pid_t);
    let (tx, rx) = mpsc::channel();
    let streams: [(bool, Box<dyn Read + Send>); 2] =
        [(true, Box::new(child.stdout.take()?)), (false, Box::new(child.stderr.take()?))];
    for (is_stdout, mut stream) in streams {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stream.read_to_end(&mut buf);
            let _ = tx.send((is_stdout, buf));
        });
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    // SAFETY (both uses): the child leads its own group, so this signals only it and its descendants.
    let kill_group = || unsafe { libc::kill(group, libc::SIGKILL) };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if deadline.is_some_and(|d| Instant::now() >= d) => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    };
    // The command is done; its output normally is too. If something it started still holds a
    // pipe, wait no longer than the limit (or a moment, without one), then end it.
    let until = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(2));
    let (mut stdout, mut stderr, mut got) = (Vec::new(), Vec::new(), 0);
    let mut killed = false;
    while got < 2 {
        let wait = if killed { Duration::from_millis(500) } else { until.saturating_duration_since(Instant::now()) };
        match rx.recv_timeout(wait) {
            Ok((true, buf)) => (stdout, got) = (buf, got + 1),
            Ok((false, buf)) => (stderr, got) = (buf, got + 1),
            Err(_) if !killed => {
                kill_group();
                killed = true;
            }
            Err(_) => break,
        }
    }
    // Output that never arrived is not empty output: to git's callers "" means "nothing changed".
    if got < 2 {
        return None;
    }
    Some(Output {
        ok: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Bundle identifiers and names of the apps that are running, lowercased. Empty if the system
/// won't say, which only means no cache is flagged as belonging to a running app.
pub fn running_apps() -> std::collections::HashSet<String> {
    let mut cmd = Command::new("/usr/bin/lsappinfo");
    cmd.arg("list");
    run(cmd, Some(Duration::from_secs(5))).map(|o| parse_running(&o.stdout)).unwrap_or_default()
}

/// `lsappinfo list` prints a block per app: ` 3) "Slack" ASN:…` followed by `bundleID="com.…"`.
fn parse_running(list: &str) -> std::collections::HashSet<String> {
    let mut apps = std::collections::HashSet::new();
    for line in list.lines() {
        let line = line.trim_start();
        let quoted = if let Some(rest) = line.strip_prefix("bundleID=\"") {
            rest.split('"').next()
        } else if line.starts_with(|c: char| c.is_ascii_digit()) {
            line.split('"').nth(1)
        } else {
            None
        };
        if let Some(name) = quoted.filter(|n| !n.is_empty()) {
            apps.insert(name.to_lowercase());
        }
    }
    apps
}

/// Working directories of the user's running processes: a shell, editor or agent sitting in a
/// folder is the best sign that the folder is in use. The commands this process started itself
/// are left out: the `git` a scan runs inside a worktree isn't someone working in it.
pub fn process_cwds() -> Option<Vec<PathBuf>> {
    let mut cmd = Command::new("/usr/sbin/lsof");
    cmd.args(["-a", "-d", "cwd", "-FpgRn", "-u"]).arg(uid().to_string());
    // lsof exits 1 when it couldn't read some processes but lists the rest; no output at all is
    // the failure, and that is `None`: not being able to ask isn't "nothing is running".
    let out = run(cmd, Some(Duration::from_secs(8)))?;
    parse_cwds(&out.stdout, std::process::id())
}

/// `lsof -FpgRn` prints a block per process: `p<pid>`, `g<group>`, `R<parent>`, then `n<folder>`.
/// A child of `me` and everything in a group led by one (`run` gives each command its own group,
/// which its helpers stay in) is this process's own doing. `None` if nothing was listed at all.
fn parse_cwds(list: &str, me: u32) -> Option<Vec<PathBuf>> {
    let mut procs: Vec<(u32, u32, u32, PathBuf)> = Vec::new();
    let (mut pid, mut group, mut parent) = (0, 0, 0);
    for line in list.lines() {
        let (field, value) = line.split_at(line.len().min(1));
        let number = || value.parse().unwrap_or(0);
        match field {
            "p" => (pid, group, parent) = (number(), 0, 0),
            "g" => group = number(),
            "R" => parent = number(),
            "n" if value.len() > 1 => procs.push((pid, group, parent, PathBuf::from(value))),
            _ => {}
        }
    }
    if procs.is_empty() {
        return None;
    }
    let ours: std::collections::HashSet<u32> = procs.iter().filter(|p| p.2 == me).map(|p| p.0).collect();
    Some(procs.into_iter().filter(|p| !ours.contains(&p.0) && !ours.contains(&p.1)).map(|p| p.3).collect())
}

/// Whether macOS lets this process read protected folders (Mail, Safari, the Trash, other apps'
/// data). Without Full Disk Access those read as empty.
pub fn full_disk_access() -> bool {
    // Folders macOS only opens to apps with Full Disk Access. A missing one says nothing either way.
    let denied = |e: &std::io::Error| e.kind() == std::io::ErrorKind::PermissionDenied;
    let home = home();
    ![home.join(".Trash"), home.join("Library/Safari"), home.join("Library/Mail")]
        .iter()
        .any(|probe| std::fs::read_dir(probe).is_err_and(|e| denied(&e)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_limit_covers_reading_the_output() {
        // The shell exits at once, but the `sleep` it leaves behind keeps the output pipe open.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 5 & echo started"]);
        let started = Instant::now();
        let out = run(cmd, Some(Duration::from_millis(300))).expect("the command itself finished");
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
        assert!(out.ok && out.stdout.contains("started"));

        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 5"]);
        let started = Instant::now();
        assert!(run(cmd, Some(Duration::from_millis(200))).is_none(), "a command that overruns is stopped");
        assert!(started.elapsed() < Duration::from_secs(2));

        // A helper that left the process group can't be stopped, and keeps the pipe. The output
        // never fully arrived, so there is no answer: an empty one would read as "nothing changed".
        // The shell waits for the helper to have left before it answers, however slow the machine.
        let left = std::env::temp_dir().join(format!("storage-left-{}", std::process::id()));
        let _ = std::fs::remove_file(&left);
        let script = "perl -MPOSIX -e 'POSIX::setsid(); open(F, \">\", $ARGV[0]); close(F); sleep 3' \"$0\" & \
                      while [ ! -e \"$0\" ]; do sleep 0.01; done; echo ' M dirty.txt'";
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(script).arg(&left);
        let started = Instant::now();
        assert!(run(cmd, Some(Duration::from_millis(300))).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = std::fs::remove_file(&left);
    }

    #[test]
    fn our_own_commands_are_not_someone_working_in_a_folder() {
        // 100 is this process. 200 is a `git` it started in a worktree, 201 a helper of that git;
        // 300 is the user's shell in another worktree, 301 an editor it started.
        let list = "p100\ng90\nR1\nfcwd\nn/\n\
                    p200\ng200\nR100\nfcwd\nn/w/ours\n\
                    p201\ng200\nR200\nfcwd\nn/w/ours/sub\n\
                    p300\ng300\nR1\nfcwd\nn/w/theirs\n\
                    p301\ng301\nR300\nfcwd\nn/w/theirs/src\n";
        let cwds = parse_cwds(list, 100).unwrap();
        assert_eq!(cwds, ["/w/theirs", "/w/theirs/src"].map(PathBuf::from));
        // Seen from 300, only the editor it started is its own.
        let cwds = parse_cwds(list, 300).unwrap();
        assert_eq!(cwds, ["/w/ours", "/w/ours/sub", "/w/theirs"].map(PathBuf::from));
        assert!(parse_cwds("", 100).is_none(), "no answer isn't \"nothing is running\"");

        // For real: a command of ours sitting in a folder isn't listed, a stranger's is.
        let dir = std::env::temp_dir().join(format!("storage-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ours")).unwrap();
        std::fs::create_dir_all(dir.join("theirs")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let mut ours = Command::new("sleep").arg("20").current_dir(dir.join("ours")).spawn().unwrap();
        // The shell exits at once, so the `sleep` it leaves behind is nobody's child.
        let theirs = Command::new("/bin/sh")
            .args(["-c", "sleep 20 >/dev/null 2>&1 & echo $!"])
            .current_dir(dir.join("theirs"))
            .output()
            .unwrap();
        let cwds = process_cwds();
        let _ = ours.kill();
        let _ = ours.wait();
        let _ = Command::new("kill").arg(String::from_utf8_lossy(&theirs.stdout).trim()).status();
        let cwds = cwds.expect("lsof answered");
        assert!(!cwds.iter().any(|c| c.starts_with(dir.join("ours"))), "{cwds:?}");
        assert!(cwds.iter().any(|c| c.starts_with(dir.join("theirs"))), "{cwds:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn running_apps_from_lsappinfo() {
        let list = r#" 1) "loginwindow" ASN:0x0-0x2002: (in front)
    bundleID="com.apple.loginwindow"
    bundle path="/System/Library/CoreServices/loginwindow.app"
 2) "universalaccessd" ASN:0x0-0xb00b:
    bundleID=[ NULL ]
 14) "Visual Studio Code" ASN:0x0-0x1c01c:
    bundleID="com.microsoft.VSCode"
"#;
        let apps = parse_running(list);
        assert!(apps.contains("com.microsoft.vscode") && apps.contains("visual studio code"));
        assert!(apps.contains("universalaccessd") && apps.contains("com.apple.loginwindow"));
        assert_eq!(apps.len(), 5);
    }
}
