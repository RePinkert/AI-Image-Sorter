use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

pub fn ensure_unique_dest(dest_dir: &Path, filename: &str) -> PathBuf {
    let mut target = dest_dir.join(filename);
    if !target.exists() {
        return target;
    }
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("img");
    let ext = Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png");
    let mut i = 1;
    loop {
        target = dest_dir.join(format!("{}_{}.{}", stem, i, ext));
        if !target.exists() {
            return target;
        }
        i += 1;
    }
}

pub fn copy_to(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let filename = src
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("image.png");
    let target = ensure_unique_dest(dest_dir, filename);
    fs::copy(src, &target)?;
    Ok(target)
}
