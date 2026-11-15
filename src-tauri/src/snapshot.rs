//! Native WebKit capture for the isolated debug UI harness. No desktop/login state is changed.
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::NSImage;
use objc2_foundation::NSError;
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};
use std::path::PathBuf;
use std::time::Duration;

pub async fn capture(window: tauri::WebviewWindow) -> Result<(), String> {
    if std::env::var_os("STORAGE_UI_SCRIPT").is_none() {
        return Err("Native snapshot is available only in the isolated UI harness".into());
    }
    let path = std::env::var_os("STORAGE_UI_SNAPSHOT")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("An absolute test snapshot output path is required")?;
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .with_webview(move |platform| {
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = tx.send(Err("Snapshot must run on the main thread".into()));
                return;
            };
            // SAFETY: Tauri's macOS platform handle is a live WKWebView; with_webview dispatches
            // here on its main thread. WebKit owns the callback's image/error for the callback.
            let webview: &WKWebView = unsafe { &*platform.inner().cast() };
            // SAFETY: the snapshot configuration is created and used on WebKit's main thread.
            let configuration = unsafe { WKSnapshotConfiguration::new(mtm) };
            // Capture the current rendered view even when the desktop is locked/occluded, rather
            // than waiting for a future screen-compositor update which might never occur.
            unsafe {
                configuration.setAfterScreenUpdates(false);
            }
            let callback = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                let result = (|| {
                    if !error.is_null() {
                        return Err("WebKit could not capture the native view".into());
                    }
                    let image = unsafe { image.as_ref() }.ok_or("WebKit returned no snapshot image")?;
                    let data = image.TIFFRepresentation().ok_or("Native snapshot has no bitmap data")?;
                    // SAFETY: this immutable NSData is retained locally and not mutated while read.
                    std::fs::write(&path, unsafe { data.as_bytes_unchecked() }).map_err(|e| e.to_string())
                })();
                let _ = tx.send(result);
            });
            unsafe {
                webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &callback);
            }
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "Native snapshot timed out".to_owned())?
}
