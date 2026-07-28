use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

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

#[allow(dead_code)]
pub fn read_first_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; 16];
    f.read_exact(&mut buf)?;
    Ok(buf)
}
