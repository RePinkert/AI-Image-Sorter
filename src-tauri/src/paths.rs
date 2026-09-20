use std::path::PathBuf;
use tauri::Manager;

pub const PORTABLE_MARKER: &str = "ai-image-sorter.portable";

pub fn portable_marker_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    Some(dir.join(PORTABLE_MARKER))
}

pub fn is_portable() -> bool {
    portable_marker_path()
        .map(|p| p.is_file())
        .unwrap_or(false)
}

/// Root for every piece of app data (DB, backups, logs, webview profile).
/// Portable mode: exe_dir/data. Installed mode: OS app-data dir.
pub fn data_root(app: &tauri::AppHandle) -> PathBuf {
    if is_portable() {
        let root = exe_dir()
            .map(|d| d.join("data"))
            .unwrap_or_else(|| PathBuf::from("data"));
        let _ = std::fs::create_dir_all(&root);
        root
    } else {
        let dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| PathBuf::from("app_data"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}

pub fn log_root_for_portable() -> Option<PathBuf> {
    is_portable().then(|| {
        let root = exe_dir()
            .map(|d| d.join("data"))
            .unwrap_or_else(|| PathBuf::from("data"));
        let logs = root.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        logs
    })
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())
}

/// Best-effort wipe of the WebView2 profile (cache, cookies, storage).
/// Portable builds keep the OS-default user-data location at runtime but
/// erase it on shutdown, so a removable drive still leaves nothing behind on
/// the host. Failures are logged, never fatal.
pub fn clear_default_webview_data(app: &tauri::AppHandle) {
    let identifier = app.config().identifier.clone();
    let dir = app
        .path()
        .resolve(&identifier, tauri::path::BaseDirectory::LocalData);
    let Ok(dir) = dir else {
        log::warn!("portable: cannot resolve default webview data dir");
        return;
    };
    match std::fs::remove_dir_all(&dir) {
        Ok(_) => log::info!("portable: default webview data cleared ({})", dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            // WebView2 child may still hold locks right at exit; one short
            // retry pass is usually enough on Windows.
            std::thread::sleep(std::time::Duration::from_millis(150));
            match std::fs::remove_dir_all(&dir) {
                Ok(_) => log::info!("portable: default webview data cleared after retry ({})", dir.display()),
                Err(e2) => log::warn!("portable: could not clear default webview data ({dir:?}): {e}; retry: {e2}"),
            }
        }
    }
}
