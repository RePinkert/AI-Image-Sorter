use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use sysinfo::System;

pub struct FoundSource {
    pub path: PathBuf,
    pub kind: &'static str, // "comfy" | "local"
    pub origin: String,
}

pub fn find_comfy_outputs() -> Vec<FoundSource> {
    let mut out: Vec<FoundSource> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    let mut add = |p: PathBuf, origin: &str| {
        if p.is_dir() {
            let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
            if seen.insert(canonical.clone()) {
                out.push(FoundSource {
                    path: canonical,
                    kind: "comfy",
                    origin: origin.to_string(),
                });
            }
        }
    };

    // 1. Common install locations.
    let candidates = [
        r"C:\ComfyUI\output",
        r"C:\ComfyUI_windows_portable\ComfyUI\output",
        r"C:\cc\ComfyUI_windows_portable\ComfyUI\output",
        r"D:\ComfyUI\output",
        r"D:\ComfyUI_windows_portable\ComfyUI\output",
    ];
    for c in candidates {
        add(PathBuf::from(c), "common-path");
    }
    if let Some(home) = dirs_home() {
        add(home.join("ComfyUI").join("output"), "user-home");
    }

    // 2. Running ComfyUI processes' command line --output-directory.
    let sys = System::new_all();
    for (_pid, proc) in sys.processes() {
        let cmd: Vec<String> = proc
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let exe = cmd.first().cloned().unwrap_or_default();
        let lower = exe.to_lowercase();
        if lower.contains("comfyui") || lower.contains("python") {
            // scan args for --output-directory
            let mut iter = cmd.iter().skip(1);
            while let Some(arg) = iter.next() {
                if arg == "--output-directory" {
                    if let Some(val) = iter.next() {
                        add(PathBuf::from(val), "process-cli");
                    }
                } else if let Some(rest) = arg.strip_prefix("--output-directory=") {
                    add(PathBuf::from(rest), "process-cli");
                }
            }
        }
    }

    out
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn list_images_in(dir: &Path) -> Vec<PathBuf> {
    list_images_with_newest(dir).0
}

/// One-pass directory walk that returns the image paths AND the newest
/// file mtime (epoch seconds). Avoids the previous two-phase pattern of
/// walking the tree and then `stat`-ing every file again just to compute
/// the change-detection watermark — both come out of the same traversal.
pub fn list_images_with_newest(dir: &Path) -> (Vec<PathBuf>, i64) {
    let mut files = Vec::new();
    let mut newest: i64 = 0;
    let exts = ["png", "jpg", "jpeg", "webp"];
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if !exts.contains(&ext.as_str()) {
            continue;
        }
        files.push(path.to_path_buf());
        if let Ok(md) = entry.metadata() {
            if let Ok(modified) = md.modified() {
                if let Ok(age) = modified.duration_since(UNIX_EPOCH) {
                    let m = age.as_secs() as i64;
                    if m > newest {
                        newest = m;
                    }
                }
            }
        }
    }
    files.sort();
    (files, newest)
}
