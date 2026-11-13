//! Storage: the Tauri shell around `storage-core`.
//!
//! The UI never passes paths to act on. `clean` takes entry keys from the last scan, and the
//! backend looks the entries up itself, so the webview can't ask for anything a scan didn't find.

use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use storage_core::clean::{self, Method};
use storage_core::sys::{self, Disk};
use storage_core::{Cat, Entry, Progress, Report, Scanner, rules};
use tauri::State;
use tauri::ipc::Channel;

#[derive(Default)]
struct AppState {
    report: Mutex<Option<Report>>,
    busy: Arc<AtomicBool>,
    /// The scan in progress, so it can be cancelled.
    scanner: Mutex<Option<Arc<Scanner>>>,
    /// Set to stop a cleanup once the item it's on is done.
    stop_clean: Arc<AtomicBool>,
}

/// What `scan` fails with when it was cancelled. The UI goes back to the previous results.
const CANCELLED: &str = "cancelled";

/// Marks the app busy for one scan or cleanup; refuses to start a second.
struct Busy(Arc<AtomicBool>);

impl Busy {
    fn take(flag: &Arc<AtomicBool>) -> Result<Self, String> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Busy(flag.clone()))
            .map_err(|_| "Already working on a scan or cleanup.".to_owned())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Progress while scanning, plus the entries found since the last one, so the list fills as it goes.
#[derive(Clone, Serialize)]
struct ScanEvent {
    #[serde(flatten)]
    progress: Progress,
    entries: Vec<Entry>,
}

#[tauri::command]
async fn scan(state: State<'_, AppState>, on_progress: Channel<ScanEvent>) -> Result<Report, String> {
    let busy = Busy::take(&state.busy)?;
    let scanner = Arc::new(Scanner::new(sys::home()).map_err(|e| format!("Can't read your home folder: {e}"))?);
    *state.scanner.lock().unwrap() = Some(scanner.clone());
    let worker = {
        let scanner = scanner.clone();
        tauri::async_runtime::spawn_blocking(move || scanner.run())
    };
    let ticker = {
        let scanner = scanner.clone();
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let handle = std::thread::spawn(move || {
            let mut sent = 0;
            while !stop.load(Ordering::Acquire) {
                let entries = scanner.entries_from(sent);
                sent += entries.len();
                if on_progress.send(ScanEvent { progress: scanner.progress(), entries }).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(90));
            }
        });
        (done, handle)
    };
    let report = worker.await.map_err(|e| format!("The scan stopped unexpectedly: {e}"));
    ticker.0.store(true, Ordering::Release);
    let _ = ticker.1.join();
    *state.scanner.lock().unwrap() = None;
    let report = report?;
    if scanner.cancelled() {
        // The last full report stays, so what's on screen can still be acted on.
        return Err(CANCELLED.into());
    }
    *state.report.lock().unwrap() = Some(report.clone());
    drop(busy);
    Ok(report)
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum CleanEvent {
    Start { key: String },
    Done { key: String, method: Method },
    Failed { key: String, error: String },
}

#[derive(Serialize)]
struct CleanSummary {
    deleted: u64,
    trashed: u64,
    failed: usize,
    /// Stopped early; the items that weren't reached are untouched.
    stopped: bool,
    /// Capacity after the cleanup, as the system reports it.
    disk: Disk,
}

/// Stops the scan or cleanup that's running. A cleanup finishes the item it's on first.
#[tauri::command]
fn cancel(state: State<'_, AppState>) {
    if let Some(scanner) = state.scanner.lock().unwrap().as_ref() {
        scanner.cancel();
    }
    state.stop_clean.store(true, Ordering::Release);
}

#[tauri::command]
async fn clean(
    state: State<'_, AppState>,
    keys: Vec<String>,
    on_progress: Channel<CleanEvent>,
) -> Result<CleanSummary, String> {
    let _busy = Busy::take(&state.busy)?;
    let wanted: HashSet<String> = keys.into_iter().collect();
    let mut entries: Vec<Entry> = {
        let guard = state.report.lock().unwrap();
        let report = guard.as_ref().ok_or("Scan first.")?;
        report
            .entries
            .iter()
            // Skip anything inside a worktree that's going as a whole.
            .filter(|e| wanted.contains(&e.key) && !e.parent.as_ref().is_some_and(|p| wanted.contains(p)))
            .cloned()
            .collect()
    };
    clean::plan(&mut entries);
    let home = sys::home();
    let stop = state.stop_clean.clone();
    stop.store(false, Ordering::Release);
    let (results, moved) = tauri::async_runtime::spawn_blocking(move || {
        let mut results = Vec::new();
        // Where what went to the Trash is now, for emptying it when the Trash can't be listed.
        let mut moved = Vec::new();
        for e in entries {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let _ = on_progress.send(CleanEvent::Start { key: e.key.clone() });
            // Driven by a UI script against a throwaway home (debug builds), the Trash is the one in
            // that home: the system's would be the real user's, whatever HOME says.
            #[cfg(debug_assertions)]
            let outcome = if std::env::var_os("STORAGE_UI_SCRIPT").is_some() {
                let trash = home.join(".Trash");
                let into_trash = |p: &std::path::Path| {
                    let name = p.file_name().ok_or("no name")?;
                    std::fs::rename(p, trash.join(name)).map_err(|e| e.to_string())
                };
                clean::clean_noting(&home, &e, &into_trash, &mut moved)
            } else {
                clean::clean_noting(&home, &e, &clean::move_to_trash, &mut moved)
            };
            #[cfg(not(debug_assertions))]
            let outcome = clean::clean_noting(&home, &e, &clean::move_to_trash, &mut moved);
            log_cleanup(&e, &outcome);
            let event = match &outcome {
                Ok(method) => CleanEvent::Done { key: e.key.clone(), method: *method },
                Err(error) => CleanEvent::Failed { key: e.key.clone(), error: error.clone() },
            };
            let _ = on_progress.send(event);
            results.push((e, outcome));
        }
        (results, moved)
    })
    .await
    .map_err(|e| format!("Cleanup stopped unexpectedly: {e}"))?;

    let stopped = state.stop_clean.load(Ordering::Acquire);
    let mut summary = CleanSummary { deleted: 0, trashed: 0, failed: 0, stopped, disk: sys::disk(&sys::home()) };
    let mut gone = HashSet::new();
    for (e, outcome) in &results {
        match outcome {
            Ok(Method::Trash) => summary.trashed += e.size,
            Ok(_) => summary.deleted += e.size,
            Err(_) => {
                summary.failed += 1;
                continue;
            }
        }
        gone.insert(e.key.clone());
    }
    if let Some(report) = state.report.lock().unwrap().as_mut() {
        report.entries.retain(|e| !gone.contains(&e.key) && !e.parent.as_ref().is_some_and(|p| gone.contains(p)));
        // What went to the Trash can now be emptied without a rescan. If the scan couldn't look
        // inside the Trash (no Full Disk Access), emptying it from here removes what Storage put there.
        if summary.trashed > 0 {
            let key = rules::key("trash", ".Trash");
            match report.entries.iter_mut().find(|e| e.key == key) {
                Some(t) => {
                    t.size += summary.trashed;
                    if let Some(items) = &mut t.trashed {
                        items.extend(moved);
                    }
                }
                None => report.entries.push(Entry {
                    key,
                    rec: "trash",
                    path: ".Trash".into(),
                    size: summary.trashed,
                    cat: Cat::Trash,
                    abs: sys::home().join(".Trash"),
                    trashed: (!report.trash_readable).then_some(moved),
                    ..Default::default()
                }),
            }
        }
    }
    Ok(summary)
}

/// Where the record of what was cleared is kept.
fn log_path() -> PathBuf {
    sys::home().join("Library/Logs/Storage/cleanup.log")
}

/// One line per item: when, how, how big, where, and the error if it failed. Never fails the cleanup.
fn log_cleanup(e: &Entry, outcome: &Result<Method, String>) {
    use std::io::Write;
    let path = log_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    let what = e.cmd.as_ref().map(|c| c.join(" ")).unwrap_or_else(|| e.abs.display().to_string());
    let result = match outcome {
        Ok(method) => format!("{method:?}").to_lowercase(),
        Err(err) => format!("FAILED ({err})"),
    };
    let _ = writeln!(file, "{}\t{}\t{}\t{}\t{}", sys::now(), result, e.rec, e.size, what);
}

/// Shows the cleanup log in Finder.
#[tauri::command]
fn reveal_log() -> Result<(), String> {
    show_in_finder(log_path())
}

#[tauri::command]
fn disk() -> Disk {
    sys::disk(&sys::home())
}

/// The recommendation table, so entries can be listed before the first report arrives.
#[tauri::command]
fn recs() -> &'static [rules::Rec] {
    rules::RECS
}

/// Whether Storage has Full Disk Access. Asked before the first scan, to explain what's missing.
#[tauri::command]
fn full_disk_access() -> bool {
    sys::full_disk_access()
}

/// Shows an entry in Finder.
#[tauri::command]
fn reveal(state: State<'_, AppState>, key: String) -> Result<(), String> {
    let path = {
        let guard = state.report.lock().unwrap();
        let report = guard.as_ref().ok_or("Scan first.")?;
        report.entries.iter().find(|e| e.key == key).map(|e| e.abs.clone()).ok_or("Not found.")?
    };
    show_in_finder(path)
}

/// Shows a folder from the drill-down view in Finder: home-relative, or absolute outside home.
/// Only reveals, so any existing path is fine.
#[tauri::command]
fn reveal_path(path: String) -> Result<(), String> {
    let path = if path.starts_with('/') { PathBuf::from(path) } else { sys::home().join(path) };
    show_in_finder(path)
}

fn show_in_finder(path: PathBuf) -> Result<(), String> {
    let target = if path.exists() { path } else { path.parent().map(ToOwned::to_owned).unwrap_or(path) };
    Command::new("open").arg("-R").arg(target).spawn().map(drop).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_privacy_settings() -> Result<(), String> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Debug builds only: what a UI script (see `STORAGE_UI_SCRIPT`) found, written to the file named
/// by `STORAGE_UI_REPORT`. A release build ignores it.
#[tauri::command]
fn dev_report(text: String) {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("STORAGE_UI_REPORT") {
        let _ = std::fs::write(path, text);
    }
    #[cfg(not(debug_assertions))]
    drop(text);
}

fn main() {
    let builder = tauri::Builder::default();
    // Debug builds can be driven end to end: `HOME=<a throwaway home> STORAGE_UI_SCRIPT=<file.js>`
    // runs that script in the window once the page has loaded, against the real backend.
    #[cfg(debug_assertions)]
    let builder = builder.on_page_load(|webview, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished
            && let Some(script) = std::env::var_os("STORAGE_UI_SCRIPT").and_then(|p| std::fs::read_to_string(p).ok())
        {
            let _ = webview.eval(script);
        }
    });
    builder
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            scan,
            clean,
            cancel,
            disk,
            recs,
            full_disk_access,
            reveal,
            reveal_path,
            reveal_log,
            dev_report,
            open_privacy_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running Storage");
}
