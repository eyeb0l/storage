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
}

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

#[tauri::command]
async fn scan(state: State<'_, AppState>, on_progress: Channel<Progress>) -> Result<Report, String> {
    let busy = Busy::take(&state.busy)?;
    let scanner = Arc::new(Scanner::new(sys::home()).map_err(|e| format!("Can't read your home folder: {e}"))?);
    let worker = {
        let scanner = scanner.clone();
        tauri::async_runtime::spawn_blocking(move || scanner.run())
    };
    let ticker = {
        let scanner = scanner.clone();
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let handle = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if on_progress.send(scanner.progress()).is_err() {
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
    let report = report?;
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
    disk: Disk,
}

#[tauri::command]
async fn clean(
    state: State<'_, AppState>,
    keys: Vec<String>,
    on_progress: Channel<CleanEvent>,
) -> Result<CleanSummary, String> {
    let _busy = Busy::take(&state.busy)?;
    let wanted: HashSet<String> = keys.into_iter().collect();
    let entries: Vec<Entry> = {
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
    let home = sys::home();
    let results = tauri::async_runtime::spawn_blocking(move || {
        let mut results = Vec::new();
        for e in entries {
            let _ = on_progress.send(CleanEvent::Start { key: e.key.clone() });
            let outcome = clean::clean(&home, &e);
            let event = match &outcome {
                Ok(method) => CleanEvent::Done { key: e.key.clone(), method: *method },
                Err(error) => CleanEvent::Failed { key: e.key.clone(), error: error.clone() },
            };
            let _ = on_progress.send(event);
            results.push((e, outcome));
        }
        results
    })
    .await
    .map_err(|e| format!("Cleanup stopped unexpectedly: {e}"))?;

    let mut summary = CleanSummary { deleted: 0, trashed: 0, failed: 0, disk: sys::disk(&sys::home()) };
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
        // What went to the Trash can now be emptied without a rescan.
        if summary.trashed > 0 {
            let key = rules::key("trash", ".Trash");
            match report.entries.iter_mut().find(|e| e.key == key) {
                Some(t) => t.size += summary.trashed,
                None => report.entries.push(Entry {
                    key,
                    rec: "trash",
                    path: ".Trash".into(),
                    size: summary.trashed,
                    cat: Cat::Trash,
                    label: None,
                    idle_days: None,
                    mtime: None,
                    cmd: None,
                    parent: None,
                    worktree: None,
                    abs: sys::home().join(".Trash"),
                    gitdir: None,
                }),
            }
        }
    }
    Ok(summary)
}

#[tauri::command]
fn disk() -> Disk {
    sys::disk(&sys::home())
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

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![scan, clean, disk, reveal, reveal_path, open_privacy_settings])
        .run(tauri::generate_context!())
        .expect("error while running Storage");
}
