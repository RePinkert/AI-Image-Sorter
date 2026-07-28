use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageMeta {
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub checkpoint: String,
    pub loras: Vec<LoraInfo>,
    pub vae: String,
    pub samplers: Vec<SamplerInfo>,
    pub raw_ok: bool,
    pub source_kind: SourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Comfy,
    A1111,
    Local,
}

impl Default for SourceKind {
    fn default() -> Self {
        SourceKind::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoraInfo {
    pub name: String,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SamplerInfo {
    pub sampler: String,
    pub seed: i64,
    pub steps: i64,
    pub cfg: f64,
    pub scheduler: String,
}

pub fn empty_comfy() -> ImageMeta {
    ImageMeta {
        source_kind: SourceKind::Comfy,
        ..Default::default()
    }
}

pub mod a1111_parse;
pub mod comfy_parse;
pub mod jpeg;
pub mod png;

use anyhow::Result;
use std::path::Path;

pub fn parse_file(path: &Path) -> Result<ImageMeta> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let mut meta = match ext.as_str() {
        "png" => png::parse_png(path)?,
        "jpg" | "jpeg" | "webp" => jpeg::parse_jpeg_webp(path)?,
        _ => ImageMeta::default(),
    };
    // If no prompt was found, try companion .txt file (Save Text node output).
    if meta.prompt_pos.is_empty() && meta.prompt_neg.is_empty() {
        if let Some(txt_meta) = try_companion_txt(path) {
            if !txt_meta.prompt_pos.is_empty() || !txt_meta.prompt_neg.is_empty() {
                if meta.prompt_pos.is_empty() {
                    meta.prompt_pos = txt_meta.prompt_pos;
                }
                if meta.prompt_neg.is_empty() {
                    meta.prompt_neg = txt_meta.prompt_neg;
                }
                if !txt_meta.raw_ok {
                    meta.raw_ok = true;
                }
                if meta.source_kind == SourceKind::Local {
                    meta.source_kind = SourceKind::Comfy;
                }
            }
        }
    }
    Ok(meta)
}

/// Try to read a companion .txt file (same stem as the image) produced by ComfyUI "Save Text" node.
fn try_companion_txt(image_path: &Path) -> Option<ImageMeta> {
    let stem = image_path.file_stem()?;
    let parent = image_path.parent()?;
    let txt_path = parent.join(format!("{}.txt", stem.to_string_lossy()));
    if !txt_path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&txt_path).ok()?;
    // The txt may contain: prompt text, or "Positive: ...\nNegative: ..." or A1111-style.
    if content.contains("Steps:") {
        return Some(a1111_parse::parse_a1111_parameters(&content));
    }
    let mut meta = ImageMeta {
        source_kind: SourceKind::Comfy,
        ..Default::default()
    };
    meta.raw_ok = true;
    // Try to split positive/negative by markers.
    if let Some(idx) = content.find("Negative prompt:") {
        meta.prompt_pos = content[..idx].trim().to_string();
        meta.prompt_neg = content[idx + 16..].trim().to_string();
    } else if let Some(idx) = content.find("Negative:") {
        meta.prompt_pos = content[..idx].trim().to_string();
        meta.prompt_neg = content[idx + 9..].trim().to_string();
    } else {
        meta.prompt_pos = content.trim().to_string();
    }
    Some(meta)
}
